// Pure rendering helpers.

import { state, NOTES_PEER } from "./state.js";
import {
  avatarColor,
  initial,
  formatTime,
  formatDate,
} from "./utils.js";

export function showChatView() {
  document.getElementById("chat-view").hidden = false;
  document.getElementById("pending-view").hidden = true;
}

export function showPendingView() {
  document.getElementById("chat-view").hidden = true;
  document.getElementById("pending-view").hidden = false;
  renderPendingList();
}

export function renderMyStatus() {
  const el = document.getElementById("me-status");
  if (!el) return;
  el.dataset.status = state.myStatus;
  document.getElementById("me-status-label").textContent = state.myStatus;
}

export function renderSidebar() {
  const peersEl = document.getElementById("peers");
  peersEl.innerHTML = "";

  const all = new Set([...state.peers, ...state.pending]);
  // Notes always present.
  if (state.me) all.add(NOTES_PEER);

  if (all.size === 0) {
    const empty = document.createElement("div");
    empty.className = "hint";
    empty.style.padding = "8px 6px";
    empty.textContent = "No chats yet.";
    peersEl.appendChild(empty);
  } else {
    // Sort: notes first, then pending, then by name.
    const sorted = [...all].sort((a, b) => {
      if (a === NOTES_PEER) return -1;
      if (b === NOTES_PEER) return 1;
      const pa = state.pending.has(a) ? 0 : 1;
      const pb = state.pending.has(b) ? 0 : 1;
      if (pa !== pb) return pa - pb;
      return a.localeCompare(b);
    });

    for (const p of sorted) {
      const d = document.createElement("div");
      const isNotes = p === NOTES_PEER;
      const status = state.peerStatus[p] || (state.online.has(p) ? "online" : null);
      const isOnline = isNotes || state.online.has(p) || state.pending.has(p);
      d.className =
        "peer" +
        (p === state.currentPeer ? " active" : "") +
        (isOnline ? " online" : " offline") +
        (status === "away" ? " status-away" : "") +
        (status === "busy" ? " status-busy" : "");
      d.dataset.peer = p;

      const av = document.createElement("div");
      av.className = "peer-avatar";
      av.style.background = isNotes ? "#8b5cf6" : avatarColor(p);
      av.textContent = isNotes ? "📝" : initial(p);

      const body = document.createElement("div");
      body.className = "peer-body";
      const name = document.createElement("div");
      name.className = "peer-name";
      name.textContent = isNotes ? "Notes" : p;
      body.appendChild(name);

      d.appendChild(av);
      d.appendChild(body);

      const unread = state.unread[p] || 0;
      if (unread > 0) {
        const b = document.createElement("span");
        b.className = "peer-unread";
        b.textContent = String(unread);
        d.appendChild(b);
      } else if (state.pending.has(p)) {
        const b = document.createElement("span");
        b.className = "badge";
        b.textContent = "new";
        d.appendChild(b);
      }

      peersEl.appendChild(d);
    }
  }

  const countEl = document.getElementById("pending-count");
  countEl.textContent = String(state.pending.size);
  countEl.dataset.empty = state.pending.size === 0 ? "true" : "false";

  // Header state
  const titleEl = document.getElementById("peer-title");
  const statusEl = document.getElementById("peer-status");
  const peerAvatar = document.getElementById("peer-avatar");

  if (state.currentPeer) {
    const isNotes = state.currentPeer === NOTES_PEER;
    titleEl.textContent = isNotes ? "Notes" : state.currentPeer;
    peerAvatar.textContent = isNotes ? "📝" : initial(state.currentPeer);
    peerAvatar.style.background = isNotes
      ? "#8b5cf6"
      : avatarColor(state.currentPeer);
    if (isNotes) {
      statusEl.textContent = "local only";
      statusEl.classList.remove("online", "away", "busy");
    } else {
      const st = state.peerStatus[state.currentPeer];
      if (state.online.has(state.currentPeer)) {
        statusEl.textContent = st && st !== "online" ? st : "online";
        statusEl.classList.toggle("online", !st || st === "online");
        statusEl.classList.toggle("away", st === "away");
        statusEl.classList.toggle("busy", st === "busy");
      } else {
        statusEl.textContent = "offline";
        statusEl.classList.remove("online", "away", "busy");
      }
    }
  } else {
    titleEl.textContent = "Select a chat";
    peerAvatar.textContent = "?";
    peerAvatar.style.background = "var(--border-strong)";
    statusEl.textContent = "";
    statusEl.classList.remove("online", "away", "busy");
  }
}

export function renderMessages() {
  const el = document.getElementById("messages");
  el.innerHTML = "";

  if (!state.currentPeer) {
    el.appendChild(
      emptyState("💬", "No chat selected", "Pick a chat from the sidebar or start a new one.")
    );
    return;
  }

  const msgs = (state.msgCache[state.currentPeer] || [])
    .slice()
    .sort((a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts);

  if (msgs.length === 0) {
    el.appendChild(
      emptyState(
        state.currentPeer === NOTES_PEER ? "📝" : "📭",
        state.currentPeer === NOTES_PEER ? "No notes yet" : "No messages yet",
        state.currentPeer === NOTES_PEER
          ? "Anything you type here stays on this device."
          : `Say hi to ${state.currentPeer}.`,
      )
    );
    return;
  }

  let lastDate = null;
  for (const m of msgs) {
    const dateStr = formatDate(m.ts);
    if (dateStr !== lastDate) {
      const sep = document.createElement("div");
      sep.className = "date-sep";
      sep.textContent = dateStr;
      el.appendChild(sep);
      lastDate = dateStr;
    }
    el.appendChild(messageEl(m, state.currentPeer === NOTES_PEER));
  }

  el.scrollTop = el.scrollHeight;
}

function emptyState(icon, title, sub) {
  const d = document.createElement("div");
  d.className = "empty-state";

  const i = document.createElement("div");
  i.className = "empty-icon";
  i.textContent = icon;

  const t = document.createElement("div");
  t.className = "empty-title";
  t.textContent = title;

  const s = document.createElement("div");
  s.className = "empty-sub";
  s.textContent = sub;

  d.appendChild(i);
  d.appendChild(t);
  d.appendChild(s);
  return d;
}

function messageEl(m, isNotes) {
  const div = document.createElement("div");
  const deleted = m.payload === "";
  div.className =
    "msg " +
    (m.direction === "out" ? "out" : "in") +
    (deleted ? " deleted" : "");
  div.dataset.id = m.id;

  const body = document.createElement("div");
  body.className = "msg-body";
  body.textContent = deleted ? "(deleted)" : m.payload;

  const time = document.createElement("div");
  time.className = "msg-time";
  time.textContent = formatTime(m.ts);

  div.appendChild(body);
  div.appendChild(time);

  if (m.direction === "out" && !deleted) {
    const actions = document.createElement("div");
    actions.className = "msg-actions";

    const editBtn = document.createElement("button");
    editBtn.dataset.act = "edit";
    editBtn.title = "Edit";
    editBtn.textContent = "✏️";

    const delBtn = document.createElement("button");
    delBtn.dataset.act = "del";
    delBtn.title = "Delete";
    delBtn.textContent = "🗑";

    actions.appendChild(editBtn);
    actions.appendChild(delBtn);
    div.appendChild(actions);
  }

  // Silence unused warning for isNotes — kept for future use.
  void isNotes;
  return div;
}

export function renderPendingList() {
  const el = document.getElementById("pending-list");
  el.innerHTML = "";

  if (state.pending.size === 0) {
    const p = document.createElement("div");
    p.className = "hint";
    p.textContent = "No pending chats.";
    el.appendChild(p);
    return;
  }

  for (const p of state.pending) {
    const row = document.createElement("div");
    row.className = "pending-row";

    const left = document.createElement("div");
    left.className = "pending-left";

    const av = document.createElement("div");
    av.className = "peer-avatar";
    av.style.background = avatarColor(p);
    av.textContent = initial(p);
    av.style.position = "relative";

    const name = document.createElement("span");
    name.className = "name";
    name.textContent = p;

    left.appendChild(av);
    left.appendChild(name);

    const btn = document.createElement("button");
    btn.className = "btn btn-primary btn-sm";
    btn.textContent = "Pull history";
    btn.dataset.accept = p;

    row.appendChild(left);
    row.appendChild(btn);
    el.appendChild(row);
  }
}