"""
End-to-end tests for the chat server.

The fixture builds and starts the Rust server on a non-standard port and
points the DB file at a temporary location, then tears everything down.
"""

from __future__ import annotations

import asyncio
import json
import os
import subprocess
import time
import uuid
from pathlib import Path

import pytest
import requests
import websockets
import websockets.exceptions

PORT = 18080
BASE = f"http://127.0.0.1:{PORT}"
WS_URL = f"ws://127.0.0.1:{PORT}/ws"

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SERVER_DIR = PROJECT_ROOT / "server"


# --------------------------------------------------------------------------- #
# fixtures                                                                    #
# --------------------------------------------------------------------------- #

@pytest.fixture(scope="session", autouse=True)
def server():
    db_file = SERVER_DIR / "chat.db"
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(db_file) + suffix)
        if p.exists():
            p.unlink()

    # Build first so the "wait for readiness" step has a real process to wait for.
    subprocess.run(["cargo", "build"], cwd=SERVER_DIR, check=True)

    env = os.environ.copy()
    env["PORT"] = str(PORT)
    env["CHAT_DB"] = str(db_file)

    proc = subprocess.Popen(
        ["cargo", "run", "--quiet"],
        cwd=SERVER_DIR,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    # Wait until /login responds (any status is fine, we just want connectivity).
    deadline = time.time() + 60
    while time.time() < deadline:
        try:
            requests.post(
                f"{BASE}/login",
                json={"username": "_probe", "password": "_probe"},
                timeout=0.5,
            )
            break
        except requests.ConnectionError:
            time.sleep(0.2)
    else:
        proc.kill()
        out = proc.stdout.read().decode(errors="replace") if proc.stdout else ""
        raise RuntimeError(f"server did not start in time:\n{out}")

    yield

    proc.terminate()
    try:
        proc.wait(timeout=5)
    except subprocess.TimeoutExpired:
        proc.kill()


# --------------------------------------------------------------------------- #
# helpers                                                                     #
# --------------------------------------------------------------------------- #

def unique_user() -> str:
    return "user_" + uuid.uuid4().hex[:10]


def register(username: str, password: str) -> requests.Response:
    return requests.post(
        f"{BASE}/register",
        json={"username": username, "password": password},
        timeout=5,
    )


def login(username: str, password: str) -> requests.Response:
    return requests.post(
        f"{BASE}/login",
        json={"username": username, "password": password},
        timeout=5,
    )


async def open_ws(username: str, password: str):
    headers = {"Authorization": f"{username}:{password}"}
    return await websockets.connect(WS_URL, additional_headers=headers)


async def recv_type(ws, msg_type: str, timeout: float = 5.0) -> dict:
    """Read messages from `ws` until one of type `msg_type` shows up."""
    deadline = time.time() + timeout
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            raise asyncio.TimeoutError(f"did not see {msg_type!r}")
        raw = await asyncio.wait_for(ws.recv(), timeout=remaining)
        msg = json.loads(raw)
        if msg.get("type") == msg_type:
            return msg


async def expect_silence(ws, timeout: float = 0.6) -> None:
    """Assert that `ws` receives nothing in the given window."""
    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(ws.recv(), timeout=timeout)


async def drain_handshake(ws):
    """Consume auth_ok, peers, pending_chats."""
    await recv_type(ws, "auth_ok")
    await recv_type(ws, "peers")
    await recv_type(ws, "pending_chats")


# --------------------------------------------------------------------------- #
# HTTP auth tests                                                             #
# --------------------------------------------------------------------------- #

def test_register_and_login():
    name = unique_user()
    assert register(name, "hunter2").status_code == 200
    # Duplicate registration.
    assert register(name, "hunter2").status_code == 409
    # Correct credentials.
    assert login(name, "hunter2").status_code == 200
    # Wrong password.
    assert login(name, "wrong").status_code == 401
    # Unknown user.
    assert login(unique_user(), "x").status_code == 401


def test_register_requires_fields():
    r = requests.post(f"{BASE}/register", json={"username": "", "password": ""})
    assert r.status_code == 400


# --------------------------------------------------------------------------- #
# websocket auth                                                              #
# --------------------------------------------------------------------------- #

async def test_ws_rejects_missing_auth():
    with pytest.raises(Exception):
        async with websockets.connect(WS_URL):
            pass


async def test_ws_rejects_wrong_password():
    name = unique_user()
    register(name, "correct")
    with pytest.raises(Exception):
        async with await open_ws(name, "wrong"):
            pass


# --------------------------------------------------------------------------- #
# chat handshake                                                              #
# --------------------------------------------------------------------------- #

async def test_first_message_is_not_delivered_until_pulled():
    a, b = unique_user(), unique_user()
    register(a, "pw")
    register(b, "pw")

    async with await open_ws(a, "pw") as wa:
        await drain_handshake(wa)

        async with await open_ws(b, "pw") as wb:
            await drain_handshake(wb)

            # `a` sends the first message to `b`.
            await wa.send(json.dumps({"type": "send", "to": b, "payload": "hi b"}))
            # Server should tell `a` the chat is pending.
            err = await recv_type(wa, "error")
            assert "initiated" in err["msg"]

            # `b` must NOT receive the message.
            await expect_silence(wb)

            # `b` asks for pending chats.
            await wb.send(json.dumps({"type": "list_pending"}))
            pending = await recv_type(wb, "pending_chats")
            assert pending["users"] == [a]

            # `b` pulls history from `a`.
            await wb.send(
                json.dumps({"type": "pull_history", "from": a, "since": 0})
            )

            req = await recv_type(wa, "pull_history_request")
            assert req["from"] == b

            # `a` answers with the first message from its local store.
            await wa.send(
                json.dumps(
                    {
                        "type": "history_response",
                        "to": b,
                        "messages": [
                            {"from": a, "to": b, "ts": 1, "payload": "hi b"}
                        ],
                    }
                )
            )

            resp = await recv_type(wb, "history_response")
            assert resp["from"] == a
            assert len(resp["messages"]) == 1
            assert resp["messages"][0]["payload"] == "hi b"

            # Now both sides are peers and direct routing works.
            await wa.send(json.dumps({"type": "send", "to": b, "payload": "second"}))
            m = await recv_type(wb, "message")
            assert m["from"] == a
            assert m["payload"] == "second"

            # And in the other direction.
            await wb.send(json.dumps({"type": "send", "to": a, "payload": "reply"}))
            m = await recv_type(wa, "message")
            assert m["from"] == b
            assert m["payload"] == "reply"


async def test_pull_history_without_initiation_is_refused():
    a, b = unique_user(), unique_user()
    register(a, "pw")
    register(b, "pw")

    async with await open_ws(a, "pw") as wa:
        await drain_handshake(wa)
        async with await open_ws(b, "pw") as wb:
            await drain_handshake(wb)

            # `b` tries to pull from `a` even though `a` never messaged them.
            await wb.send(
                json.dumps({"type": "pull_history", "from": a, "since": 0})
            )
            err = await recv_type(wb, "error")
            assert "has not messaged" in err["msg"]


async def test_cannot_send_to_unknown_user():
    a = unique_user()
    register(a, "pw")

    async with await open_ws(a, "pw") as wa:
        await drain_handshake(wa)

        # Send to a user that doesn't exist / never interacted: the server
        # treats it as a pending initiation, but refuses to open the chat.
        await wa.send(json.dumps({"type": "send", "to": "ghost", "payload": "?"}))
        err = await recv_type(wa, "error")
        assert "ghost" in err["msg"]


async def test_offline_peer_pull_returns_error():
    a, b = unique_user(), unique_user()
    register(a, "pw")
    register(b, "pw")

    # `a` initiates while `b` is offline.
    async with await open_ws(a, "pw") as wa:
        await drain_handshake(wa)
        await wa.send(json.dumps({"type": "send", "to": b, "payload": "hi"}))
        await recv_type(wa, "error")

    # `b` connects later, sees the pending chat, but `a` is now offline.
    async with await open_ws(b, "pw") as wb:
        await drain_handshake(wb)
        await wb.send(
            json.dumps({"type": "pull_history", "from": a, "since": 0})
        )
        err = await recv_type(wb, "error")
        assert "offline" in err["msg"]


# --------------------------------------------------------------------------- #
# account management over websocket                                           #
# --------------------------------------------------------------------------- #

async def test_delete_account_over_websocket():
    name = unique_user()
    register(name, "pw")

    async with await open_ws(name, "pw") as ws:
        await drain_handshake(ws)
        await ws.send(
            json.dumps({"type": "delete_account", "password": "pw"})
        )
        close = await recv_type(ws, "close")
        assert close["type"] == "close"

    # Account is gone.
    assert login(name, "pw").status_code == 401
    # Registering the same name again works.
    assert register(name, "pw").status_code == 200


async def test_delete_account_wrong_password():
    name = unique_user()
    register(name, "pw")

    async with await open_ws(name, "pw") as ws:
        await drain_handshake(ws)
        await ws.send(
            json.dumps({"type": "delete_account", "password": "nope"})
        )
        err = await recv_type(ws, "error")
        assert "password" in err["msg"]

    # Still able to log in.
    assert login(name, "pw").status_code == 200

async def test_second_connection_is_rejected():
    name = unique_user()
    register(name, "pw")

    async with await open_ws(name, "pw") as first:
        await drain_handshake(first)

        # A second login with the same credentials must not be allowed.
        async with await open_ws(name, "pw") as second:
            err = json.loads(await asyncio.wait_for(second.recv(), timeout=5))
            assert err["type"] == "error"
            assert "already connected" in err["msg"]

            # The server then closes the socket.
            with pytest.raises(websockets.exceptions.ConnectionClosed):
                await asyncio.wait_for(second.recv(), timeout=2)

        # The first session is still fully functional.
        await first.send(json.dumps({"type": "list_pending"}))
        pending = await recv_type(first, "pending_chats")
        assert isinstance(pending["users"], list)


async def test_slot_freed_after_disconnect():
    name = unique_user()
    register(name, "pw")

    async with await open_ws(name, "pw") as first:
        await drain_handshake(first)
    # Let the server run its cleanup block.
    await asyncio.sleep(0.3)

    # Reconnecting with the same user now succeeds.
    async with await open_ws(name, "pw") as second:
        msg = await recv_type(second, "auth_ok")
        assert msg["username"] == name
