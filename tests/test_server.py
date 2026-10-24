"""
End-to-end tests for the chat server and protocol.

Tests cover:
1. HTTP registration and input validation.
2. WebSocket authentication (handshake, invalid credentials, missing headers).
3. Initial presence and profile synchronization.
4. First message initiation (pending chat, manual pull, no unprompted notifications).
5. The CRITICAL EDGE CASE:
   - User 1 messages User 2 (offline)
   - User 1 goes offline
   - User 2 messages User 1 (offline)
   - User 2 goes offline
   - Both User 1 and User 2 come online
   - History and presence sync cleanly in both directions.
6. Multi-session (multi-device) support for the same account.
7. WebSocket keepalive (Ping / Pong heartbeat).
8. Non-destructive blocking and unblocking.
9. Account deletion over WebSocket.
"""

from __future__ import annotations

import asyncio
import inspect
import json
import os
import subprocess
import time
import uuid
from pathlib import Path

import pytest
import requests
import websockets

PORT = 18080
BASE = f"http://127.0.0.1:{PORT}"
WS_URL = f"ws://127.0.0.1:{PORT}/login"

PROJECT_ROOT = Path(__file__).resolve().parent.parent
SERVER_DIR = PROJECT_ROOT / "server"


# --------------------------------------------------------------------------- #
# Fixtures                                                                    #
# --------------------------------------------------------------------------- #

@pytest.fixture(scope="session", autouse=True)
def server():
    db_file = Path(__file__).resolve().parent / "test_chat.db"
    for suffix in ("", "-wal", "-shm"):
        p = Path(str(db_file) + suffix)
        if p.exists():
            try:
                p.unlink()
            except OSError:
                pass

    env = os.environ.copy()
    env["PATH"] = r"C:\Program Files\JetBrains\CLion 2025.3.2\bin\mingw\bin;" + env.get("PATH", "")
    env["PORT"] = str(PORT)
    env["CHAT_DB"] = str(db_file)

    # Build server binary
    subprocess.run(["cargo", "build"], cwd=SERVER_DIR, env=env, check=True)

    server_exe = SERVER_DIR / "target" / "debug" / "chat-server.exe"

    proc = subprocess.Popen(
        [str(server_exe)],
        cwd=SERVER_DIR,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
    )

    deadline = time.time() + 30
    while time.time() < deadline:
        try:
            requests.get(f"{BASE}/login", timeout=0.5)
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
# Helpers                                                                     #
# --------------------------------------------------------------------------- #

def unique_user() -> str:
    return "user_" + uuid.uuid4().hex[:10]


def register(username: str, password: str = "password123") -> requests.Response:
    return requests.post(
        f"{BASE}/register",
        json={"username": username, "password": password},
        timeout=5,
    )


def open_ws(username: str, password: str = "password123"):
    headers = {"Authorization": f"{username}:{password}"}
    # Support both websockets 13.x (extra_headers) and 14+ (additional_headers)
    sig = inspect.signature(websockets.connect)
    if "extra_headers" in sig.parameters:
        return websockets.connect(WS_URL, extra_headers=headers)
    return websockets.connect(WS_URL, additional_headers=headers)


async def recv_type(ws, msg_type: str, timeout: float = 5.0) -> dict:
    deadline = time.time() + timeout
    while True:
        remaining = deadline - time.time()
        if remaining <= 0:
            raise asyncio.TimeoutError(f"did not see {msg_type!r}")
        raw = await asyncio.wait_for(ws.recv(), timeout=remaining)
        if isinstance(raw, bytes):
            continue
        msg = json.loads(raw)
        if msg.get("type") == msg_type:
            return msg


async def expect_silence(ws, timeout: float = 0.5) -> None:
    with pytest.raises(asyncio.TimeoutError):
        await asyncio.wait_for(ws.recv(), timeout=timeout)


async def drain_handshake(ws) -> dict:
    """Consume initial connection messages and return auth_ok payload."""
    auth = await recv_type(ws, "auth_ok")
    await recv_type(ws, "status_update")
    await recv_type(ws, "peers")
    await recv_type(ws, "pending_chats")
    await recv_type(ws, "blocked")
    return auth


# --------------------------------------------------------------------------- #
# Tests                                                                       #
# --------------------------------------------------------------------------- #

def test_register_flow():
    u = unique_user()
    r1 = register(u, "secret")
    assert r1.status_code == 200
    assert r1.json() == {"status": "ok"}

    # Duplicate
    r2 = register(u, "secret")
    assert r2.status_code == 409

    # Empty
    r3 = register("", "")
    assert r3.status_code == 400


@pytest.mark.asyncio
async def test_ws_auth():
    u = unique_user()
    register(u, "correct")

    # Missing auth
    with pytest.raises(Exception):
        async with websockets.connect(WS_URL):
            pass

    # Wrong password
    with pytest.raises(Exception):
        async with open_ws(u, "wrong"):
            pass

    # Success
    async with open_ws(u, "correct") as ws:
        auth = await drain_handshake(ws)
        assert auth["username"] == u
        assert isinstance(auth["user_id"], int)


@pytest.mark.asyncio
async def test_first_message_is_pending_until_manually_pulled():
    u1, u2 = unique_user(), unique_user()
    register(u1, "pw")
    register(u2, "pw")

    async with open_ws(u1, "pw") as w1:
        auth1 = await drain_handshake(w1)
        id1 = auth1["user_id"]

        async with open_ws(u2, "pw") as w2:
            auth2 = await drain_handshake(w2)
            id2 = auth2["user_id"]

            # User 1 sends first message to User 2
            await w1.send(json.dumps({
                "type": "send",
                "to": id2,
                "id": "msg_001",
                "ts": 1000,
                "kind": "text",
                "payload": "Hello from u1"
            }))

            # User 2 must NOT receive the message directly!
            await expect_silence(w2, timeout=0.6)

            # User 2 lists pending chats
            await w2.send(json.dumps({"type": "list_pending"}))
            pending = await recv_type(w2, "pending_chats")
            assert any(p["id"] == id1 for p in pending["users"])

            # User 2 manually pulls history from User 1
            await w2.send(json.dumps({
                "type": "pull_history",
                "from": id1,
                "since": 0
            }))

            # User 1 receives pull_history_request
            req = await recv_type(w1, "pull_history_request")
            assert req["from"] == id2

            # User 1 answers with the message from its store
            await w1.send(json.dumps({
                "type": "history_response",
                "to": id2,
                "messages": [
                    {
                        "id": "msg_001",
                        "from": id1,
                        "to": id2,
                        "ts": 1000,
                        "edit_ts": 1000,
                        "kind": "text",
                        "payload": "Hello from u1"
                    }
                ]
            }))

            # User 2 receives history_response
            resp = await recv_type(w2, "history_response")
            assert resp["from"] == id1
            assert len(resp["messages"]) == 1
            assert resp["messages"][0]["payload"] == "Hello from u1"

            # Direct connection is now established
            await w1.send(json.dumps({
                "type": "send",
                "to": id2,
                "id": "msg_002",
                "ts": 2000,
                "kind": "text",
                "payload": "Direct hello"
            }))
            direct_msg = await recv_type(w2, "message")
            assert direct_msg["from"] == id1
            assert direct_msg["payload"] == "Direct hello"


@pytest.mark.asyncio
async def test_very_edge_case_offline_bidirectional_sync():
    """
    REMEMBER VERY EDGE CASE IN MIND:
    user 1 online msg -> user 2 offline,
    then user 1 offline,
    then user 2 online msg -> user 1 offline,
    then both go online.
    message should sync properly!!! (and online/offline status immediately, too).
    """
    u1, u2 = unique_user(), unique_user()
    register(u1, "pw")
    register(u2, "pw")

    # Step 0: Establish relationship initially
    async with open_ws(u1, "pw") as w1:
        id1 = (await drain_handshake(w1))["user_id"]
        async with open_ws(u2, "pw") as w2:
            id2 = (await drain_handshake(w2))["user_id"]
            await w1.send(json.dumps({
                "type": "send", "to": id2, "id": "m0", "ts": 100, "kind": "text", "payload": "init"
            }))
            await asyncio.sleep(0.05)
            await w2.send(json.dumps({"type": "pull_history", "from": id1, "since": 0}))
            await recv_type(w1, "pull_history_request")
            await w1.send(json.dumps({
                "type": "history_response", "to": id2, "messages": [
                    {"id": "m0", "from": id1, "to": id2, "ts": 100, "edit_ts": 100, "kind": "text", "payload": "init"}
                ]
            }))
            await recv_type(w2, "history_response")

    # Step 1: User 1 is online, User 2 is offline. User 1 sends M1.
    async with open_ws(u1, "pw") as w1:
        await drain_handshake(w1)
        # User 1 sends M1 to User 2 (who is offline)
        await w1.send(json.dumps({
            "type": "send",
            "to": id2,
            "id": "m1_u1_to_u2",
            "ts": 1000,
            "kind": "text",
            "payload": "Message 1 from U1 while U2 was offline"
        }))
        await asyncio.sleep(0.1)

    # Step 2: User 1 goes offline (w1 context closed).

    # Step 3: User 2 comes online while User 1 is offline. User 2 sends M2.
    async with open_ws(u2, "pw") as w2:
        await drain_handshake(w2)
        # User 2 sends M2 to User 1 (who is offline)
        await w2.send(json.dumps({
            "type": "send",
            "to": id1,
            "id": "m2_u2_to_u1",
            "ts": 2000,
            "kind": "text",
            "payload": "Message 2 from U2 while U1 was offline"
        }))
        await asyncio.sleep(0.1)

    # Step 4: User 2 goes offline (w2 context closed).

    # Step 5: Both User 1 and User 2 come online!
    async with open_ws(u1, "pw") as w1:
        await drain_handshake(w1)

        async with open_ws(u2, "pw") as w2:
            await drain_handshake(w2)

            # Both must immediately receive peer_online for each other!
            onl_for_u1 = await recv_type(w1, "peer_online")
            assert onl_for_u1["user_id"] == id2
            assert onl_for_u1["status"] == "online"

            onl_for_u2 = await recv_type(w2, "peer_online")
            assert onl_for_u2["user_id"] == id1
            assert onl_for_u2["status"] == "online"

            # User 1 requests history from User 2
            await w1.send(json.dumps({"type": "pull_history", "from": id2, "since": 0}))
            # User 2 gets the pull request and responds with M2
            req_u2 = await recv_type(w2, "pull_history_request")
            assert req_u2["from"] == id1
            await w2.send(json.dumps({
                "type": "history_response",
                "to": id1,
                "messages": [
                    {
                        "id": "m2_u2_to_u1",
                        "from": id2,
                        "to": id1,
                        "ts": 2000,
                        "edit_ts": 2000,
                        "kind": "text",
                        "payload": "Message 2 from U2 while U1 was offline"
                    }
                ]
            }))
            # User 1 receives M2!
            resp_u1 = await recv_type(w1, "history_response")
            assert resp_u1["messages"][0]["id"] == "m2_u2_to_u1"

            # User 2 requests history from User 1
            await w2.send(json.dumps({"type": "pull_history", "from": id1, "since": 0}))
            # User 1 gets the pull request and responds with M1
            req_u1 = await recv_type(w1, "pull_history_request")
            assert req_u1["from"] == id2
            await w1.send(json.dumps({
                "type": "history_response",
                "to": id2,
                "messages": [
                    {
                        "id": "m1_u1_to_u2",
                        "from": id1,
                        "to": id2,
                        "ts": 1000,
                        "edit_ts": 1000,
                        "kind": "text",
                        "payload": "Message 1 from U1 while U2 was offline"
                    }
                ]
            }))
            # User 2 receives M1!
            resp_u2 = await recv_type(w2, "history_response")
            assert resp_u2["messages"][0]["id"] == "m1_u1_to_u2"


@pytest.mark.asyncio
async def test_heartbeat_ping_pong():
    u = unique_user()
    register(u, "pw")

    async with open_ws(u, "pw") as ws:
        await drain_handshake(ws)

        # Send Ping frame from client
        pong_waiter = await ws.ping(b"heartbeat_check")
        # Ensure pong arrives
        await asyncio.wait_for(pong_waiter, timeout=3.0)


@pytest.mark.asyncio
async def test_multi_session_same_user():
    u = unique_user()
    register(u, "pw")

    async with open_ws(u, "pw") as s1:
        await drain_handshake(s1)

        # Same user opens second session (e.g. desktop + laptop)
        async with open_ws(u, "pw") as s2:
            await drain_handshake(s2)

            # Note message to self sent from s1
            await s1.send(json.dumps({
                "type": "send_to_self",
                "id": "self_note_1",
                "ts": 1000,
                "edit_ts": 1000,
                "kind": "text",
                "payload": "Synced Note"
            }))

            # Session 2 must receive the note message!
            note = await recv_type(s2, "note_message")
            assert note["id"] == "self_note_1"
            assert note["payload"] == "Synced Note"


@pytest.mark.asyncio
async def test_block_unblock_behavior():
    u1, u2 = unique_user(), unique_user()
    register(u1, "pw")
    register(u2, "pw")

    async with open_ws(u1, "pw") as w1:
        id1 = (await drain_handshake(w1))["user_id"]
        async with open_ws(u2, "pw") as w2:
            id2 = (await drain_handshake(w2))["user_id"]

            # Establish chat
            await w1.send(json.dumps({"type": "send", "to": id2, "id": "m1", "ts": 1, "kind": "text", "payload": "hi"}))
            await asyncio.sleep(0.05)
            await w2.send(json.dumps({"type": "pull_history", "from": id1, "since": 0}))
            await recv_type(w1, "pull_history_request")
            await w1.send(json.dumps({
                "type": "history_response", "to": id2, "messages": [
                    {"id": "m1", "from": id1, "to": id2, "ts": 1, "edit_ts": 1, "kind": "text", "payload": "hi"}
                ]
            }))
            await recv_type(w2, "history_response")

            # User 1 blocks User 2
            await w1.send(json.dumps({"type": "block_user", "user_id": id2}))
            blocked = await recv_type(w1, "blocked")
            assert any(p["id"] == id2 for p in blocked["users"])

            # User 2 tries to send message to User 1 -> blocked
            await w2.send(json.dumps({"type": "send", "to": id1, "id": "m2", "ts": 2, "kind": "text", "payload": "blocked?"}))
            await expect_silence(w1, timeout=0.5)

            # User 1 tries to send message to User 2 -> also blocked
            await w1.send(json.dumps({"type": "send", "to": id2, "id": "m2b", "ts": 2, "kind": "text", "payload": "blocked_out?"}))
            await expect_silence(w2, timeout=0.5)

            # User 1 unblocks User 2
            await w1.send(json.dumps({"type": "unblock_user", "user_id": id2}))
            blocked_after = await recv_type(w1, "blocked")
            assert not any(p["id"] == id2 for p in blocked_after["users"])

            # User 2 can now send messages again
            await w2.send(json.dumps({"type": "send", "to": id1, "id": "m3", "ts": 3, "kind": "text", "payload": "unblocked!"}))
            msg = await recv_type(w1, "message")
            assert msg["payload"] == "unblocked!"


@pytest.mark.asyncio
async def test_delete_account_over_websocket():
    name = unique_user()
    register(name, "pw")

    async with open_ws(name, "pw") as ws:
        await drain_handshake(ws)
        await ws.send(json.dumps({"type": "delete_account", "password": "pw"}))
        close = await recv_type(ws, "close")
        assert close["reason"] == "account_deleted"

    # Re-registering with the same name now works because old account is deleted
    assert register(name, "new_pw").status_code == 200

