// Tauri event listeners — translate server pushes into UI updates.

import { invoke, listen } from "./api.js";
import { state } from "./state.js";
import { toast, avatarColor, initial } from "./utils.js";
import {
  renderSidebar,
  renderMessages,
  renderPendingList,
  showChatView,
} from "./ui.js";
import { autoPull, resetToLogin } from "./actions.js";
import { refreshEncryptionStatus } from "./encryption.js";

export async function setupEvents() {
  await listen("auth-ok", (e) => {
    state.me = e.payload.username;

    const nameEl = document.getElementById("me-name");
    nameEl.textContent = state.me;

    const av = document.getElementById("me-avatar");
    av.textContent = initial(state.me);
    av.style.background = avatarColor(state.me);

    document.getElementById("login-view").hidden = true;
    document.getElementById("app-view").hidden = false;

    showChatView();
    renderSidebar();
    renderMessages();
  });

  await listen("peers", (e) => {
    for (const p of e.payload) state.peers.add(p);
    renderSidebar();
    // Kick off history pull for every peer we know about.
    for (const p of e.payload) autoPull(p);
  });

  await listen("pending-chats", (e) => {
    state.pending.clear();
    for (const p of e.payload) {
      if (!state.peers.has(p)) state.pending.add(p);
    }
    renderSidebar();
    renderPendingList();
  });

  await listen("peer-online", (e) => {
    const name = e.payload;
    state.online.add(name);
    // Reset stale "pulling" so a previously-failed pull can be retried.
    state.pulling.delete(name);
    if (!state.pending.has(name)) state.peers.add(name);
    renderSidebar();
    if (!state.pending.has(name)) autoPull(name);
  });

  await listen("peer-offline", (e) => {
    state.online.delete(e.payload);
    renderSidebar();
  });

  await listen("message", (e) => {
    const m = e.payload;
    state.pending.delete(m.peer);
    state.peers.add(m.peer);

    if (!state.msgCache[m.peer]) state.msgCache[m.peer] = [];
    const idx = state.msgCache[m.peer].findIndex((x) => x.id === m.id);

    const entry = {
      id: m.id,
      direction: m.direction,
      ts: m.ts,
      edit_ts: m.edit_ts,
      kind: m.kind,
      payload: m.payload,
    };

    if (idx >= 0) state.msgCache[m.peer][idx] = entry;
    else state.msgCache[m.peer].push(entry);

    renderSidebar();
    renderPendingList();
    if (state.currentPeer === m.peer) renderMessages();
  });

  await listen("history-received", async (e) => {
    const peer = e.payload;
    state.pulling.delete(peer);

    try {
      state.msgCache[peer] = await invoke("get_messages", { peer });
    } catch (_) {
      state.msgCache[peer] = state.msgCache[peer] || [];
    }

    if (state.pending.has(peer)) {
      state.pending.delete(peer);
      state.peers.add(peer);
      renderSidebar();
      renderPendingList();
    }
    if (state.currentPeer === peer) renderMessages();
  });

  await listen("error", (e) => {
    const msg = String(e.payload);

    // "peer <name> is offline, try again later" — only clear the specific
    // peer's guard so other in-flight pulls stay throttled.
    const m = msg.match(/^peer (.+?) is offline/);
    if (m) {
      state.pulling.delete(m[1]);
      return;
    }
    toast(msg);
  });

  await listen("session-closed", async (e) => {
    const reason = String(e.payload || "");

    if (reason === "account_deleted") {
      alert("Account deleted.");
      try { await invoke("wipe_local_data"); } catch (_) {}
      await refreshEncryptionStatus();
    } else if (reason === "session_taken_over") {
      alert("Session closed: you signed in from another window or device.");
    } else {
      alert("Session closed by server.");
    }

    try { await invoke("disconnect"); } catch (_) {}
    resetToLogin();
  });

  await listen("disconnected", () => {
    if (state.me) resetToLogin();
  });
}