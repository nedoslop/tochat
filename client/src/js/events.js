// Tauri event listeners.

import { invoke, listen } from "./api.js";
import { state, NOTES_PEER } from "./state.js";
import { toast, avatarColor, initial } from "./utils.js";
import { showAlert } from "./dialog.js";
import {
  renderSidebar,
  renderMessages,
  renderPendingList,
  renderMyStatus,
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

    renderMyStatus();
    showChatView();
    renderSidebar();
    renderMessages();
  });

  await listen("peers", (e) => {
    for (const p of e.payload) state.peers.add(p);
    // No bulk pull here — chats are fetched lazily.
    renderSidebar();
  });

  await listen("pending-chats", (e) => {
    state.pending.clear();
    for (const p of e.payload) {
      if (!state.peers.has(p)) state.pending.add(p);
    }
    renderSidebar();
    renderPendingList();
  });

  await listen("blocked", (e) => {
    state.blocked.clear();
    for (const p of e.payload) state.blocked.add(p);
    renderSidebar();
  });

  await listen("peer-online", (e) => {
    const name = e.payload;
    state.online.add(name);
    state.peerStatus[name] = state.peerStatus[name] || "online";
    state.pulling.delete(name);
    if (!state.pending.has(name)) state.peers.add(name);
    renderSidebar();
    // Only auto-sync peers we already have local history with, or the
    // currently-open chat. Everything else waits for the user to open it.
    const engaged =
      name === state.currentPeer ||
      (state.msgCache[name] && state.msgCache[name].length > 0);
    if (!state.pending.has(name) && engaged) autoPull(name);
  });

  await listen("peer-offline", (e) => {
    state.online.delete(e.payload);
    delete state.peerStatus[e.payload];
    renderSidebar();
  });

  await listen("status-update", (e) => {
    const { username, status } = e.payload;
    state.peerStatus[username] = status;
    if (status === "invisible") {
      state.online.delete(username);
    } else {
      state.online.add(username);
    }
    renderSidebar();
  });

  await listen("chat-left", (e) => {
    const peer = e.payload;
    state.peers.delete(peer);
    state.pending.delete(peer);
    delete state.msgCache[peer];
    delete state.mightHaveMore[peer];
    delete state.lastPullLimit[peer];
    state.unread[peer] = 0;
    if (state.currentPeer === peer) {
      state.currentPeer = null;
      renderMessages();
    }
    renderSidebar();
    toast(`Chat with ${peer} was closed.`);
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

    if (m.direction === "in" && state.currentPeer !== m.peer) {
      state.unread[m.peer] = (state.unread[m.peer] || 0) + 1;
    }

    renderSidebar();
    renderPendingList();
    if (state.currentPeer === m.peer) renderMessages();
  });

  await listen("history-received", async (e) => {
    const peer = e.payload;
    state.pulling.delete(peer);

    const prevLen = (state.msgCache[peer] || []).length;
    try {
      state.msgCache[peer] = await invoke("get_messages", { peer });
    } catch (_) {
      state.msgCache[peer] = state.msgCache[peer] || [];
    }
    const newLen = state.msgCache[peer].length;
    const added = newLen - prevLen;

    // If we requested a bounded page and got fewer rows than the limit,
    // we've reached the beginning of history.
    const limit = state.lastPullLimit[peer];
    if (limit != null) {
      state.mightHaveMore[peer] = added >= limit;
      delete state.lastPullLimit[peer];
    }
    state.loadingOlder.delete(peer);

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

    const m = msg.match(/^peer (.+?) is offline/);
    if (m) {
      state.pulling.delete(m[1]);
      state.loadingOlder.delete(m[1]);
      return;
    }
    toast(msg);
  });

  await listen("session-closed", async (e) => {
    const reason = String(e.payload || "");

    if (reason === "account_deleted") {
      await showAlert("Your account has been deleted.", {
        title: "Account deleted",
      });
      try { await invoke("wipe_local_data"); } catch (_) {}
      await refreshEncryptionStatus();
    } else if (reason === "session_taken_over") {
      await showAlert(
        "You were signed in from another window or device. This session has been closed.",
        { title: "Session closed" },
      );
    } else {
      await showAlert("The server closed this session.", {
        title: "Session closed",
      });
    }

    try { await invoke("disconnect"); } catch (_) {}
    resetToLogin();
  });

  await listen("disconnected", () => {
    if (state.me) resetToLogin();
  });
}

export { NOTES_PEER };