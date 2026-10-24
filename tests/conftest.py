"""
Shared pytest fixtures and helpers for tochat server and client tests.
"""

from __future__ import annotations

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
    sig = inspect.signature(websockets.connect)
    if "extra_headers" in sig.parameters:
        return websockets.connect(WS_URL, extra_headers=headers)
    return websockets.connect(WS_URL, additional_headers=headers)


def now_ms() -> int:
    return int(time.time() * 1000)
