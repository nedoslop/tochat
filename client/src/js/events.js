import { invoke, listen } from "./api.js";
import { state, NOTES_PEER, INITIAL_LIMIT, persistMyStatus } from "./state.js";
import { toast, avatarColor, initial } from "./utils.js";
import { showAlert } from "./dialog.js";
import {
  renderSidebar, renderMessages, renderPendingList, renderBlockedList,
  renderMyStatus, showChatView, updateReadIndicators,
} from "./ui.js";
import {
  autoPull, resetToLogin, sendReadReceipt, refreshReadState, openPeer, updateBadge,
} from "./actions.js";
import { refreshEncryptionStatus } from "./encryption.js";

function applyMyAvatar() {
  const av = document.getElementById("me-avatar");
  const mine = state.profiles[state.meId];
  if (mine && mine.avatar) {
    av.innerHTML = "";
    const img = document.createElement("img");
    img.src = mine.avatar;
    img.alt = "";
    img.className = "avatar-img";
    av.appendChild(img);
    av.style.background = "var(--surface-3)";
  } else {
    av.textContent = initial(state.meName);
    av.style.background = avatarColor(state.meName);
  }
}

function mergeIntoCache(peerId, incoming) {
  const existing = state.msgCache[peerId] || [];
  const byId = new Map();
  for (const m of existing) byId.set(m.id, m);
  for (const m of incoming) byId.set(m.id, m);
  const merged = [...byId.values()].sort(
    (a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts,
  );
  state.msgCache[peerId] = merged;
}

function syncWithPeer(peerId, { delay = 0 } = {}) {
  if (!peerId || peerId === state.meId || peerId === NOTES_PEER) return;
  const run = () => { autoPull(peerId); };
  if (delay > 0) setTimeout(run, delay);
  else run();
}

async function windowIsFocused() {
  try { return await invoke("is_window_focused"); }
  catch (_) { return false; }
}

export async function setupEvents() {
  await listen("auth-ok", async (e) => {
    state.meId = e.payload.user_id;
    state.meName = e.payload.username;

    document.getElementById("me-name").textContent = state.meName;
    const av = document.getElementById("me-avatar");
    av.textContent = initial(state.meName);
    av.style.background = avatarColor(state.meName);

    document.getElementById("login-view").hidden = true;
    document.getElementById("app-view").hidden = false;

    try {
      const counts = await invoke("get_unread_counts");
      for (const [peerId, n] of counts) state.unread[peerId] = n;
    } catch (_) {}

    renderMyStatus();
    showChatView();
    renderSidebar();
    renderMessages();
    void updateBadge();
  });

  await listen("peers", async (e) => {
    for (const info of e.payload) {
      state.peers.add(info.id);
      state.nameToId.set(info.username, info.id);
      if (!state.profiles[info.id]) {
        state.profiles[info.id] = {
          username: info.username,
          display_name: null,
          avatar: null,
        };
      }
    }
    renderSidebar();

    for (const info of e.payload) {
      try {
        const msgs = await invoke("get_messages", {
          peer: info.id, beforeTs: null, limit: INITIAL_LIMIT,
        });
        mergeIntoCache(info.id, msgs);
      } catch (_) {}
      syncWithPeer(info.id);
    }
    renderSidebar();
  });

  await listen("pending-chats", (e) => {
    state.pending.clear();
    for (const info of e.payload) {
      if (!state.peers.has(info.id)) state.pending.add(info.id);
      state.nameToId.set(info.username, info.id);
      if (!state.profiles[info.id]) {
        state.profiles[info.id] = {
          username: info.username,
          display_name: null,
          avatar: null,
        };
      }
    }
    renderSidebar();
    renderPendingList();
  });

  await listen("blocked", (e) => {
    state.blocked.clear();
    for (const info of e.payload) {
      state.blocked.add(info.id);
      if (!state.profiles[info.id]) {
        state.profiles[info.id] = {
          username: info.username,
          display_name: null,
          avatar: null,
        };
      }
    }
    renderSidebar();
    renderBlockedList();
  });

  await listen("profile", (e) => {
    const { user_id, username, display_name, avatar } = e.payload;
    if (!user_id) return;
    state.profiles[user_id] = { username, display_name, avatar };
    state.nameToId.set(username, user_id);
    if (user_id === state.meId) applyMyAvatar();
    renderSidebar();
  });

  await listen("peer-online", (e) => {
    const id = e.payload.user_id;
    const status = e.payload.status || "online";
    state.peerStatus[id] = status;
    if (status === "invisible") state.online.delete(id);
    else state.online.add(id);
    renderSidebar();

    syncWithPeer(id);
    syncWithPeer(id, { delay: 500 });
    syncWithPeer(id, { delay: 2500 });

    if (state.currentPeer === id) {
      refreshReadState(id).then(() => {
        if (state.currentPeer !== id) return;
        windowIsFocused().then((focused) => {
          if (focused && state.currentPeer === id) void sendReadReceipt(id);
        });
      });
    }
  });

  await listen("peer-offline", (e) => {
    const id = e.payload.user_id;
    state.online.delete(id);
    delete state.peerStatus[id];
    renderSidebar();
  });

  await listen("status-update", (e) => {
    const { user_id, status } = e.payload;
    if (user_id === state.meId) {
      state.myStatus = status;
      persistMyStatus(status);
      renderMyStatus();
    }
    state.peerStatus[user_id] = status;
    if (status === "invisible") state.online.delete(user_id);
    else state.online.add(user_id);
    renderSidebar();
  });

  await listen("chat-left", (e) => {
    const peer = e.payload.peer;
    state.peers.delete(peer);
    state.pending.delete(peer);
    delete state.msgCache[peer];
    delete state.seenIds[peer];
    delete state.mightHaveMore[peer];
    delete state.lastPullLimit[peer];
    state.unread[peer] = 0;
    if (state.currentPeer === peer) {
      state.currentPeer = null;
      renderMessages();
    }
    renderSidebar();
    void updateBadge();
  });

  await listen("message", (e) => {
    const m = e.payload;
    const peerId = m.from;
    state.pending.delete(peerId);
    state.peers.add(peerId);

    if (!state.msgCache[peerId]) state.msgCache[peerId] = [];
    const idx = state.msgCache[peerId].findIndex((x) => x.id === m.id);
    const entry = {
      id: m.id,
      direction: "in",
      ts: m.ts,
      edit_ts: m.edit_ts,
      kind: m.kind,
      payload: m.payload,
      read: !!m.windowFocused && state.currentPeer === peerId,
    };
    if (idx >= 0) state.msgCache[peerId][idx] = entry;
    else state.msgCache[peerId].push(entry);

    const isOpen = state.currentPeer === peerId;
    if (!isOpen || !m.windowFocused) {
      state.unread[peerId] = (state.unread[peerId] || 0) + 1;
      void updateBadge();
    }

    renderSidebar();
    renderPendingList();
    if (isOpen) {
      renderMessages();
      if (m.windowFocused) {
        void sendReadReceipt(peerId);
      }
    }
  });

  await listen("note-message", async () => {
    state.peers.add(NOTES_PEER);
    try {
      state.msgCache[NOTES_PEER] = await invoke("get_messages", {
        peer: NOTES_PEER, beforeTs: null, limit: null,
      });
    } catch (_) {}
    renderSidebar();
    if (state.currentPeer === NOTES_PEER) renderMessages();
  });

  await listen("read-receipt", (e) => {
    const { peer, up_to_ts } = e.payload;
    const msgs = state.msgCache[peer] || [];
    let changed = false;
    for (const m of msgs) {
      if (m.direction === "out" && !m.read && m.ts <= up_to_ts) {
        m.read = true;
        changed = true;
      }
    }
    if (changed) updateReadIndicators(peer);
  });

  await listen("history-received", async (e) => {
    const peer = e.payload.from;

    try {
      const latest = await invoke("get_messages", {
        peer, beforeTs: null, limit: INITIAL_LIMIT,
      });
      mergeIntoCache(peer, latest);
    } catch (_) {}

    const wasLoadingOlder = state.loadingOlder.has(peer);
    delete state.lastPullLimit[peer];
    state.loadingOlder.delete(peer);

    if (wasLoadingOlder && (state.msgCache[peer] || []).length > 0) {
      const oldest = state.msgCache[peer][0].ts;
      try {
        const hasMore = await invoke("has_messages_before", { peer, beforeTs: oldest });
        state.mightHaveMore[peer] = hasMore;
      } catch (_) {}
    }

    if (state.pending.has(peer)) {
      state.pending.delete(peer);
      state.peers.add(peer);
      renderPendingList();
    }

    try {
      const counts = await invoke("get_unread_counts");
      const map = Object.fromEntries(counts.map(([id, n]) => [String(id), n]));
      state.unread[peer] = map[String(peer)] || 0;
      void updateBadge();
    } catch (_) {}

    renderSidebar();

    if (state.currentPeer === peer) {
      renderMessages(wasLoadingOlder);
      if (wasLoadingOlder) {
        requestAnimationFrame(() => { state.suppressScrollLoad = false; });
      } else {
        const focused = await windowIsFocused();
        if (focused) void sendReadReceipt(peer);
      }
    } else {
      state.suppressScrollLoad = false;
    }
  });

  await listen("error", (e) => {
    const msg = String(e.payload);
    const m = msg.match(/^peer (\d+) is offline/);
    if (m) {
      const id = Number(m[1]);
      state.loadingOlder.delete(id);
      state.suppressScrollLoad = false;
      return;
    }
    toast(msg);
  });

  await listen("in-app-notification", (e) => {
    showInAppBanner(e.payload.title, e.payload.body, e.payload.peer);
  });

  await listen("session-closed", async (e) => {
    const reason = String(e.payload || "");
    if (reason === "account_deleted") {
      await showAlert("Your account has been deleted.", { title: "Account deleted" });
      try { await invoke("wipe_local_data"); } catch (_) {}
      await refreshEncryptionStatus();
    } else if (reason === "session_taken_over") {
      await showAlert(
        "You were signed in from another window or device. This session has been closed.",
        { title: "Session closed" },
      );
    } else {
      await showAlert("The server closed this session.", { title: "Session closed" });
    }
    try { await invoke("disconnect"); } catch (_) {}
    resetToLogin();
  });

  await listen("disconnected", () => {
    if (state.meId) resetToLogin();
  });
}

// ---------- In-app notification banner ----------

let bannerTimer = null;
function showInAppBanner(title, body, peer) {
  const el = document.getElementById("in-app-banner");
  document.getElementById("in-app-banner-title").textContent = title || "Notification";
  document.getElementById("in-app-banner-text").textContent = body || "";
  el.hidden = false;
  el.onclick = (ev) => {
    if (ev.target.id === "in-app-banner-close") {
      el.hidden = true;
      clearTimeout(bannerTimer);
      return;
    }
    if (peer) {
      el.hidden = true;
      clearTimeout(bannerTimer);
      openPeer(peer);
    }
  };
  clearTimeout(bannerTimer);
  bannerTimer = setTimeout(() => { el.hidden = true; }, 6000);
}

export { NOTES_PEER };