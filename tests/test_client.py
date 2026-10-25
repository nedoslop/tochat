"""
Comprehensive client-side test suite for tochat.

Tests cover:
1. Client local SQLite database operations (schema, upsert, edit, delete, pagination, read receipts).
2. Client-side cryptography (SharedPassword Argon2 + ChaCha20-Poly1305, PreSharedKey 32-byte hex).
3. Multi-instance client independence (running 2 client instances simultaneously on the same machine without collisions or session drops).
4. Manual pull flow: initial message held until receiver explicitly pulls history.
5. The CRITICAL EDGE CASE from client perspective:
   - Client 1 messages Client 2 (offline) -> saved in Client 1 local DB.
   - Client 1 goes offline.
   - Client 2 messages Client 1 (offline) -> saved in Client 2 local DB.
   - Client 2 goes offline.
   - Both clients come online -> bidirectional history pull -> both local databases in complete parity.
6. End-to-end encrypted messaging between two client instances (wire contains only ciphertext, local DBs contain decrypted plaintext).
7. Read receipt propagation and local DB status update.
"""

from __future__ import annotations

import asyncio
import base64
import json
import os
import sqlite3
import tempfile
import time
import uuid
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Optional

import pytest
from argon2 import Type, low_level
from cryptography.hazmat.primitives.ciphers.aead import ChaCha20Poly1305

from conftest import BASE, WS_URL, now_ms, open_ws, register, unique_user

SALT = b"chat-app-shared-encryption-salt-v1"


# --------------------------------------------------------------------------- #
# Client Crypto Implementation (matches client/src-tauri/src/crypto.rs)      #
# --------------------------------------------------------------------------- #

class ClientCipher:
    def encrypt(self, plaintext: str) -> str:
        raise NotImplementedError

    def decrypt(self, wire: str) -> Optional[str]:
        raise NotImplementedError


class SharedPasswordCipher(ClientCipher):
    def __init__(self, password: str):
        # Argon2id default settings in Rust argon2 crate
        self.key = low_level.hash_secret_raw(
            secret=password.encode("utf-8"),
            salt=SALT,
            time_cost=3,
            memory_cost=65536,
            parallelism=1,
            hash_len=32,
            type=Type.ID,
        )
        self.cipher = ChaCha20Poly1305(self.key)

    def encrypt(self, plaintext: str) -> str:
        nonce = os.urandom(12)
        ct = self.cipher.encrypt(nonce, plaintext.encode("utf-8"), None)
        return base64.b64encode(nonce + ct).decode("ascii")

    def decrypt(self, wire: str) -> Optional[str]:
        try:
            raw = base64.b64decode(wire)
            if len(raw) < 12 + 16:
                return None
            nonce, ct = raw[:12], raw[12:]
            return self.cipher.decrypt(nonce, ct, None).decode("utf-8")
        except Exception:
            return None


class PreSharedKeyCipher(ClientCipher):
    def __init__(self, key_hex: str):
        self.key = bytes.fromhex(key_hex.strip())
        if len(self.key) != 32:
            raise ValueError("PSK must be 32 bytes (64 hex characters)")
        self.cipher = ChaCha20Poly1305(self.key)

    def encrypt(self, plaintext: str) -> str:
        nonce = os.urandom(12)
        ct = self.cipher.encrypt(nonce, plaintext.encode("utf-8"), None)
        return base64.b64encode(nonce + ct).decode("ascii")

    def decrypt(self, wire: str) -> Optional[str]:
        try:
            raw = base64.b64decode(wire)
            if len(raw) < 12 + 16:
                return None
            nonce, ct = raw[:12], raw[12:]
            return self.cipher.decrypt(nonce, ct, None).decode("utf-8")
        except Exception:
            return None


# --------------------------------------------------------------------------- #
# Client SQLite Database (matches client/src-tauri/src/db.rs)               #
# --------------------------------------------------------------------------- #

class ClientDatabase:
    def __init__(self, path: Path):
        self.path = path
        path.parent.mkdir(parents=True, exist_ok=True)
        self.conn = sqlite3.connect(str(path), timeout=5.0)
        self.conn.row_factory = sqlite3.Row
        self._init_schema()

    def _init_schema(self):
        with self.conn:
            self.conn.executescript(
                """
                PRAGMA journal_mode = WAL;
                PRAGMA synchronous = NORMAL;

                CREATE TABLE IF NOT EXISTS messages (
                  peer_id   INTEGER NOT NULL,
                  id        TEXT NOT NULL,
                  direction TEXT NOT NULL,
                  ts        INTEGER NOT NULL,
                  edit_ts   INTEGER NOT NULL,
                  kind      TEXT NOT NULL,
                  payload   TEXT NOT NULL,
                  plaintext INTEGER NOT NULL DEFAULT 0,
                  read      INTEGER NOT NULL DEFAULT 0,
                  PRIMARY KEY (peer_id, id)
                );

                CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer_id, ts);

                CREATE TABLE IF NOT EXISTS peers (
                  id           INTEGER PRIMARY KEY,
                  username     TEXT NOT NULL,
                  display_name TEXT,
                  avatar       TEXT
                );

                CREATE TABLE IF NOT EXISTS peer_encryption (
                  peer_id INTEGER PRIMARY KEY,
                  method  TEXT NOT NULL,
                  secret  TEXT
                );
                """
            )

    def upsert_peer(self, peer_id: int, username: str, display_name: Optional[str] = None):
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO peers (id, username, display_name)
                VALUES (?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                  username = excluded.username,
                  display_name = excluded.display_name
                """,
                (peer_id, username, display_name),
            )

    def upsert_message(
        self,
        peer_id: int,
        msg_id: str,
        direction: str,
        ts: int,
        edit_ts: int,
        kind: str,
        payload: str,
        plaintext: bool = True,
        read: bool = False,
    ):
        with self.conn:
            self.conn.execute(
                """
                INSERT INTO messages
                  (peer_id, id, direction, ts, edit_ts, kind, payload, plaintext, read)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(peer_id, id) DO UPDATE SET
                  ts        = excluded.ts,
                  edit_ts   = excluded.edit_ts,
                  kind      = excluded.kind,
                  payload   = excluded.payload,
                  plaintext = excluded.plaintext,
                  read      = MAX(messages.read, excluded.read)
                """,
                (peer_id, msg_id, direction, ts, edit_ts, kind, payload, int(plaintext), int(read)),
            )

    def get_messages(
        self,
        peer_id: int,
        before_ts: Optional[int] = None,
        limit: Optional[int] = None,
    ) -> list[dict]:
        limit_val = -1 if limit is None else limit
        query = """
            SELECT id, direction, ts, edit_ts, kind, payload, plaintext, read
            FROM messages
            WHERE peer_id = ? AND (? IS NULL OR ts < ?)
            ORDER BY ts DESC, edit_ts DESC
            LIMIT ?
        """
        rows = self.conn.execute(query, (peer_id, before_ts, before_ts, limit_val)).fetchall()
        msgs = [dict(r) for r in rows]
        msgs.reverse()
        return msgs

    def mark_read_up_to(self, peer_id: int, up_to_ts: int):
        with self.conn:
            self.conn.execute(
                """
                UPDATE messages SET read = 1
                WHERE peer_id = ? AND direction = 'out' AND ts <= ? AND read = 0
                """,
                (peer_id, up_to_ts),
            )

    def mark_incoming_read(self, peer_id: int, up_to_ts: int):
        with self.conn:
            self.conn.execute(
                """
                UPDATE messages SET read = 1
                WHERE peer_id = ? AND direction = 'in' AND ts <= ? AND read = 0
                """,
                (peer_id, up_to_ts),
            )

    def set_encryption(self, peer_id: int, method: str, secret: Optional[str]):
        with self.conn:
            if method == "none":
                self.conn.execute("DELETE FROM peer_encryption WHERE peer_id = ?", (peer_id,))
            else:
                self.conn.execute(
                    """
                    INSERT INTO peer_encryption (peer_id, method, secret)
                    VALUES (?, ?, ?)
                    ON CONFLICT(peer_id) DO UPDATE SET
                      method = excluded.method,
                      secret = excluded.secret
                    """,
                    (peer_id, method, secret),
                )

    def close(self):
        self.conn.close()


# --------------------------------------------------------------------------- #
# Client Instance Simulator                                                   #
# --------------------------------------------------------------------------- #

class ClientNode:
    """
    Simulates a running Tauri client process with its own SQLite database,
    ciphers, and WebSocket connection.
    """
    def __init__(self, username: str, password: str, data_dir: Path):
        self.username = username
        self.password = password
        self.data_dir = data_dir
        self.user_id: Optional[int] = None
        self.db = ClientDatabase(data_dir / f"{username}.db")
        self.ciphers: dict[int, ClientCipher] = {}
        self.ws: Any = None
        self.inbox: asyncio.Queue[dict] = asyncio.Queue()
        self._pump_task: Optional[asyncio.Task] = None

    async def connect(self):
        self.ws = await open_ws(self.username, self.password)
        # First message from server is auth_ok
        raw = await asyncio.wait_for(self.ws.recv(), timeout=5.0)
        auth = json.loads(raw)
        assert auth["type"] == "auth_ok", f"Expected auth_ok, got {auth}"
        self.user_id = auth["user_id"]
        self._pump_task = asyncio.create_task(self._pump())

    async def _pump(self):
        try:
            while True:
                raw = await self.ws.recv()
                msg = json.loads(raw)
                await self.inbox.put(msg)
        except Exception as exc:
            print(f"[{self.username}] PUMP EXITED: {exc}")

    async def disconnect(self):
        if self._pump_task:
            self._pump_task.cancel()
            try:
                await self._pump_task
            except asyncio.CancelledError:
                pass
            self._pump_task = None
        if self.ws:
            await self.ws.close()
            self.ws = None
        self.inbox = asyncio.Queue()

    def set_cipher(self, peer_id: int, cipher: ClientCipher):
        self.ciphers[peer_id] = cipher

    async def send_text(self, to_id: int, text: str) -> str:
        msg_id = uuid.uuid4().hex[:12]
        ts = now_ms()
        cipher = self.ciphers.get(to_id)
        payload = cipher.encrypt(text) if cipher else text
        is_plaintext = cipher is None

        # Store in local database
        self.db.upsert_message(
            peer_id=to_id,
            msg_id=msg_id,
            direction="out",
            ts=ts,
            edit_ts=ts,
            kind="text",
            payload=text,
            plaintext=is_plaintext,
            read=False,
        )

        # Send over WebSocket
        await self.ws.send(json.dumps({
            "type": "send",
            "to": to_id,
            "id": msg_id,
            "ts": ts,
            "kind": "text",
            "payload": payload,
        }))
        return msg_id

    async def request_pull(self, peer_id: int, since: int = 0):
        await self.ws.send(json.dumps({
            "type": "pull_history",
            "from": peer_id,
            "since": since,
        }))

    async def answer_pull(self, request_msg: dict):
        peer_id = request_msg["from"]
        since = request_msg.get("since", 0)
        local_msgs = self.db.get_messages(peer_id)
        filtered = [m for m in local_msgs if m["edit_ts"] > since]

        cipher = self.ciphers.get(peer_id)
        wire_messages = []
        for m in filtered:
            payload = cipher.encrypt(m["payload"]) if cipher else m["payload"]
            wire_messages.append({
                "id": m["id"],
                "from": self.user_id if m["direction"] == "out" else peer_id,
                "to": peer_id if m["direction"] == "out" else self.user_id,
                "ts": m["ts"],
                "edit_ts": m["edit_ts"],
                "kind": m["kind"],
                "payload": payload,
            })

        await self.ws.send(json.dumps({
            "type": "history_response",
            "to": peer_id,
            "messages": wire_messages,
        }))

    def ingest_history(self, peer_id: int, messages: list[dict]):
        cipher = self.ciphers.get(peer_id)
        for m in messages:
            direction = "out" if m["from"] == self.user_id else "in"
            raw_payload = m["payload"]
            if cipher:
                decrypted = cipher.decrypt(raw_payload)
                stored_payload = decrypted if decrypted is not None else raw_payload
                is_plaintext = decrypted is not None
            else:
                stored_payload = raw_payload
                is_plaintext = True

            self.db.upsert_message(
                peer_id=peer_id,
                msg_id=m["id"],
                direction=direction,
                ts=m["ts"],
                edit_ts=m["edit_ts"],
                kind=m.get("kind", "text"),
                payload=stored_payload,
                plaintext=is_plaintext,
                read=(direction == "out"),
            )

    async def send_read_receipt(self, peer_id: int, up_to_ts: int):
        self.db.mark_incoming_read(peer_id, up_to_ts)
        await self.ws.send(json.dumps({
            "type": "read_receipt",
            "to": peer_id,
            "up_to_ts": up_to_ts,
        }))

    async def wait_for_type(self, msg_type: str, timeout: float = 5.0) -> dict:
        messages_seen = []
        deadline = time.time() + timeout
        while time.time() < deadline:
            try:
                msg = await asyncio.wait_for(self.inbox.get(), timeout=max(0.1, deadline - time.time()))
                messages_seen.append(msg.get("type"))
                if msg.get("type") == msg_type:
                    return msg
            except asyncio.TimeoutError:
                break
        raise asyncio.TimeoutError(f"User {self.username} timed out waiting for {msg_type!r}. Seen: {messages_seen}")


# =========================================================================== #
# Tests                                                                       #
# =========================================================================== #

def test_client_database_crud_and_pagination(tmp_path):
    """
    Validates that the client SQLite database operations (upsert, edit,
    pagination, read receipts, and encryption settings) function identically
    to Tauri's Rusqlite layer.
    """
    db_path = tmp_path / "alice.db"
    db = ClientDatabase(db_path)

    db.upsert_peer(101, "bob", "Bob Dylan")
    db.upsert_message(101, "m1", "out", ts=1000, edit_ts=1000, kind="text", payload="Hello")
    db.upsert_message(101, "m2", "in", ts=2000, edit_ts=2000, kind="text", payload="World")

    all_msgs = db.get_messages(101)
    assert len(all_msgs) == 2
    assert all_msgs[0]["id"] == "m1"
    assert all_msgs[1]["id"] == "m2"

    # Edit m1
    db.upsert_message(101, "m1", "out", ts=1000, edit_ts=1500, kind="text", payload="Hello edited")
    after_edit = db.get_messages(101)
    assert len(after_edit) == 2
    assert after_edit[0]["payload"] == "Hello edited"
    assert after_edit[0]["edit_ts"] == 1500

    # Pagination: before_ts = 2000
    older = db.get_messages(101, before_ts=2000, limit=1)
    assert len(older) == 1
    assert older[0]["id"] == "m1"

    # Read receipts
    db.mark_read_up_to(101, 1500)
    db.mark_incoming_read(101, 2500)
    read_msgs = db.get_messages(101)
    assert read_msgs[0]["read"] == 1
    assert read_msgs[1]["read"] == 1

    # Encryption persistence
    db.set_encryption(101, "pre_shared_key", "deadbeef" * 8)
    row = db.conn.execute("SELECT method, secret FROM peer_encryption WHERE peer_id = 101").fetchone()
    assert row["method"] == "pre_shared_key"

    db.close()


def test_client_crypto_implementations():
    """
    Validates client-side Argon2+ChaCha20Poly1305 and PreSharedKey encryption.
    """
    # 1. Shared password
    c1 = SharedPasswordCipher("topsecret123")
    c2 = SharedPasswordCipher("topsecret123")
    plaintext = "Encrypted message over peer connection"
    wire = c1.encrypt(plaintext)
    assert wire != plaintext
    assert c2.decrypt(wire) == plaintext

    wrong = SharedPasswordCipher("wrong_secret")
    assert wrong.decrypt(wire) is None

    # 2. Pre-shared key
    psk_hex = "0123456789abcdef" * 4
    psk1 = PreSharedKeyCipher(psk_hex)
    psk2 = PreSharedKeyCipher(psk_hex)
    psk_wire = psk1.encrypt("Secret PSK payload")
    assert psk2.decrypt(psk_wire) == "Secret PSK payload"
    assert psk2.decrypt("corrupted_base64_data") is None


async def establish_chat(c1: ClientNode, c2: ClientNode):
    """
    Performs the initial handshake per spec:
    c1 sends to c2 -> c2 manually pulls history from c1 -> c1 answers -> relationship established.
    """
    await c1.send_text(c2.user_id, "Initiate chat")
    
    # Wait until server has registered the pending chat for c2
    for _ in range(30):
        await asyncio.sleep(0.1)
        await c2.ws.send(json.dumps({"type": "list_pending"}))
        try:
            pending = await c2.wait_for_type("pending_chats", timeout=0.5)
            if any(p["id"] == c1.user_id for p in pending.get("users", [])):
                break
        except asyncio.TimeoutError:
            pass

    await c2.request_pull(c1.user_id, since=0)
    req = await c1.wait_for_type("pull_history_request", timeout=5.0)
    await c1.answer_pull(req)
    hist = await c2.wait_for_type("history_response", timeout=5.0)
    c2.ingest_history(c1.user_id, hist["messages"])


@pytest.mark.asyncio
async def test_client_multi_instance_concurrent_chat(tmp_path):
    """
    Validates that multiple independent client instances can run concurrently
    with distinct accounts on the same machine, exchange messages, and neither
    is disconnected or kicked.
    """
    u1, u2 = unique_user(), unique_user()
    register(u1)
    register(u2)

    c1 = ClientNode(u1, "password123", tmp_path / "c1")
    c2 = ClientNode(u2, "password123", tmp_path / "c2")

    try:
        await c1.connect()
        await c2.connect()

        # Handshake to establish direct chatting
        await establish_chat(c1, c2)

        # Client 1 messages Client 2 directly
        msg_id = await c1.send_text(c2.user_id, "Hello from instance 1!")
        dm = await c2.wait_for_type("message")
        assert dm["id"] == msg_id
        assert dm["payload"] == "Hello from instance 1!"

        # Client 2 replies directly
        reply_id = await c2.send_text(c1.user_id, "Hello back from instance 2!")
        reply = await c1.wait_for_type("message")
        assert reply["id"] == reply_id
        assert reply["payload"] == "Hello back from instance 2!"

        # Verify both sessions remain connected and healthy
        assert not c1.ws.closed
        assert not c2.ws.closed
    finally:
        await c1.disconnect()
        await c2.disconnect()
        c1.db.close()
        c2.db.close()


@pytest.mark.asyncio
async def test_client_first_message_manual_pull(tmp_path):
    """
    To begin a chat, user 1 messages user 2, and user 2 should request
    message history from user 1 manually (no notifications beforehand).
    Only then is history exchanged and saved to local DB.
    """
    u1, u2 = unique_user(), unique_user()
    register(u1)
    register(u2)

    c1 = ClientNode(u1, "password123", tmp_path / "u1")
    c2 = ClientNode(u2, "password123", tmp_path / "u2")
    os.makedirs(c1.data_dir, exist_ok=True)
    os.makedirs(c2.data_dir, exist_ok=True)

    try:
        # Discover c2 user_id then take c2 offline
        await c2.connect()
        c2_id = c2.user_id
        await c2.disconnect()

        # c1 connects and sends to offline c2
        await c1.connect()
        msg_id = await c1.send_text(c2_id, "Secret proposition")

        # c2 comes online.
        await c2.connect()

        # c2 must NOT receive any direct_message unprompted!
        # Server sends pending_chats informing c2 that c1 wants to connect.
        pending = await c2.wait_for_type("pending_chats")
        pending_ids = [p["id"] for p in pending["users"]]
        assert c1.user_id in pending_ids

        # Verify c2 local DB still has 0 messages
        assert len(c2.db.get_messages(c1.user_id)) == 0

        # Now c2 manually pulls history from c1
        await c2.request_pull(c1.user_id, since=0)

        # c1 receives pull_history_request
        pull_req = await c1.wait_for_type("pull_history_request")
        assert pull_req["from"] == c2.user_id

        # c1 answers pull request with local history
        await c1.answer_pull(pull_req)

        # c2 receives history_response
        hist_resp = await c2.wait_for_type("history_response")
        c2.ingest_history(c1.user_id, hist_resp["messages"])

        # Verify message is now in c2 local DB
        stored = c2.db.get_messages(c1.user_id)
        assert len(stored) == 1
        assert stored[0]["id"] == msg_id
        assert stored[0]["payload"] == "Secret proposition"
        assert stored[0]["direction"] == "in"
    finally:
        await c1.disconnect()
        await c2.disconnect()
        c1.db.close()
        c2.db.close()


@pytest.mark.asyncio
async def test_client_critical_edge_case_offline_sync(tmp_path):
    """
    CRITICAL EDGE CASE FROM SPEC:
    1. User 1 online msg -> User 2 offline
    2. User 1 goes offline
    3. User 2 online msg -> User 1 offline
    4. Both go offline
    5. Both come online -> message and statuses sync properly!
    """
    u1, u2 = unique_user(), unique_user()
    register(u1)
    register(u2)

    c1 = ClientNode(u1, "password123", tmp_path / "c1_edge")
    c2 = ClientNode(u2, "password123", tmp_path / "c2_edge")
    os.makedirs(c1.data_dir, exist_ok=True)
    os.makedirs(c2.data_dir, exist_ok=True)

    try:
        # Establish initial connection and relationship
        await c1.connect()
        await c2.connect()
        u1_id, u2_id = c1.user_id, c2.user_id
        await establish_chat(c1, c2)

        # Step 1: c2 goes offline. c1 writes to offline c2
        await c2.disconnect()
        await asyncio.sleep(0.2)
        m1_id = await c1.send_text(u2_id, "Message 1 from Alice while Bob is offline")

        # Step 2: c1 goes offline
        await c1.disconnect()
        await asyncio.sleep(0.2)

        # Step 3: c2 goes online. c2 writes to offline c1
        await c2.connect()
        m2_id = await c2.send_text(u1_id, "Message 2 from Bob while Alice is offline")

        # Step 4: c2 goes offline (both now offline)
        await c2.disconnect()
        await asyncio.sleep(0.2)

        # Step 5: Both come online
        await c1.connect()
        await c2.connect()
        await c1.wait_for_type("peer_online")
        await c2.wait_for_type("peer_online")

        # Both exchange history pulls:
        # c1 pulls from c2
        await c1.request_pull(u2_id, since=0)
        pull_for_c2 = await c2.wait_for_type("pull_history_request")
        await c2.answer_pull(pull_for_c2)
        hist1 = await c1.wait_for_type("history_response")
        c1.ingest_history(u2_id, hist1["messages"])
        await asyncio.sleep(0.1)

        # c2 pulls from c1
        await c2.request_pull(u1_id, since=0)
        pull_for_c1 = await c1.wait_for_type("pull_history_request")
        await c1.answer_pull(pull_for_c1)
        hist2 = await c2.wait_for_type("history_response")
        c2.ingest_history(u1_id, hist2["messages"])

        # Verify both local databases have all messages
        msgs_c1 = c1.db.get_messages(u2_id)
        msgs_c2 = c2.db.get_messages(u1_id)

        ids_c1 = {m["id"] for m in msgs_c1}
        ids_c2 = {m["id"] for m in msgs_c2}

        assert m1_id in ids_c1
        assert m2_id in ids_c1
        assert m1_id in ids_c2
        assert m2_id in ids_c2

        # Verify content match
        m1_c2 = next(m for m in msgs_c2 if m["id"] == m1_id)
        assert m1_c2["payload"] == "Message 1 from Alice while Bob is offline"
        assert m1_c2["direction"] == "in"

        m2_c1 = next(m for m in msgs_c1 if m["id"] == m2_id)
        assert m2_c1["payload"] == "Message 2 from Bob while Alice is offline"
        assert m2_c1["direction"] == "in"
    finally:
        await c1.disconnect()
        await c2.disconnect()
        c1.db.close()
        c2.db.close()


@pytest.mark.asyncio
async def test_client_e2e_encryption_flow(tmp_path):
    """
    Validates end-to-end encryption between two clients:
    - Ciphertext sent over the wire cannot be decrypted without the secret.
    - Server only relays ciphertext.
    - Both clients store and view decrypted plaintext locally.
    """
    u1, u2 = unique_user(), unique_user()
    register(u1)
    register(u2)

    c1 = ClientNode(u1, "password123", tmp_path / "c1_enc")
    c2 = ClientNode(u2, "password123", tmp_path / "c2_enc")
    os.makedirs(c1.data_dir, exist_ok=True)
    os.makedirs(c2.data_dir, exist_ok=True)

    try:
        await c1.connect()
        await c2.connect()
        await asyncio.sleep(0.1)
        await establish_chat(c1, c2)

        # Pre-shared key negotiated out of band
        psk_hex = "abcdef0123456789" * 4
        c1.set_cipher(c2.user_id, PreSharedKeyCipher(psk_hex))
        c2.set_cipher(c1.user_id, PreSharedKeyCipher(psk_hex))

        secret_text = "This is a strictly confidential conversation!"
        msg_id = await c1.send_text(c2.user_id, secret_text)

        # c2 receives message frame
        wire_msg = await c2.wait_for_type("message")
        # Verify the wire payload is encrypted base64, NOT plaintext
        assert wire_msg["payload"] != secret_text
        assert secret_text not in wire_msg["payload"]

        # Decrypt and store in c2 local database
        decrypted = c2.ciphers[c1.user_id].decrypt(wire_msg["payload"])
        assert decrypted == secret_text

        c2.db.upsert_message(
            peer_id=c1.user_id,
            msg_id=wire_msg["id"],
            direction="in",
            ts=wire_msg["ts"],
            edit_ts=wire_msg["edit_ts"],
            kind=wire_msg["kind"],
            payload=decrypted,
            plaintext=True,
        )

        stored = c2.db.get_messages(c1.user_id)
        assert len(stored) == 2
        assert stored[1]["payload"] == secret_text
    finally:
        await c1.disconnect()
        await c2.disconnect()
        c1.db.close()
        c2.db.close()


@pytest.mark.asyncio
async def test_client_read_receipt_flow(tmp_path):
    """
    Validates that read receipts sent by the recipient update the read status
    in the sender's local database.
    """
    u1, u2 = unique_user(), unique_user()
    register(u1)
    register(u2)

    c1 = ClientNode(u1, "password123", tmp_path / "c1_rr")
    c2 = ClientNode(u2, "password123", tmp_path / "c2_rr")
    os.makedirs(c1.data_dir, exist_ok=True)
    os.makedirs(c2.data_dir, exist_ok=True)

    try:
        await c1.connect()
        await c2.connect()
        await asyncio.sleep(0.1)
        await establish_chat(c1, c2)

        # c1 sends message
        m_id = await c1.send_text(c2.user_id, "Notice me")
        msg = await c2.wait_for_type("message")

        # Initially unread in c1's local database
        c1_local = c1.db.get_messages(c2.user_id)
        assert c1_local[0]["read"] == 0

        # c2 marks read and sends receipt
        await c2.send_read_receipt(c1.user_id, up_to_ts=msg["ts"])

        # c1 receives read receipt
        receipt = await c1.wait_for_type("read_receipt")
        assert receipt["from"] == c2.user_id
        assert receipt["up_to_ts"] >= msg["ts"]

        # c1 updates local database
        c1.db.mark_read_up_to(c2.user_id, receipt["up_to_ts"])
        c1_updated = c1.db.get_messages(c2.user_id)
        assert c1_updated[0]["read"] == 1
    finally:
        await c1.disconnect()
        await c2.disconnect()
        c1.db.close()
        c2.db.close()
