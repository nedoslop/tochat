// Pure rendering helpers — no Tauri calls, no side effects beyond DOM.

import { state } from "./state.js";
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

export function renderSidebar() {
  const peersEl = document.getElementById("peers");
  peersEl.innerHTML = "";

  const all = new Set([...state.peers, ...state.pending]);

  if (all.size === 0) {
    const empty = document.createElement("div");
    empty.className = "hint";
    empty.style.padding = "8px 6px";
    empty.textContent = "No chats yet.";
    peersEl.appendChild(empty);
  } else {
    for (const p of all) {
      const d = document.createElement("div");
      const isOnline = state.online.has(p) || state.pending.has(p);
      d.className =
        "peer" +
        (p === state.currentPeer ? " active" : "") +
        (isOnline ? " online" : " offline");
      d.dataset.peer = p;

      const av = document.createElement("div");
      av.className = "peer-avatar";
      av.style.background = avatarColor(p);
      av.textContent = initial(p);

      const body = document.createElement("div");
      body.className = "peer-body";
      const name = document.createElement("div");
      name.className = "peer-name";
      name.textContent = p;
      body.appendChild(name);

      d.appendChild(av);
      d.appendChild(body);

      if (state.pending.has(p)) {
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
    titleEl.textContent = state.currentPeer;
    peerAvatar.textContent = initial(state.currentPeer);
    peerAvatar.style.background = avatarColor(state.currentPeer);
    if (state.online.has(state.currentPeer)) {
      statusEl.textContent = "online";
      statusEl.classList.add("online");
    } else {
      statusEl.textContent = "offline";
      statusEl.classList.remove("online");
    }
  } else {
    titleEl.textContent = "Select a chat";
    peerAvatar.textContent = "?";
    peerAvatar.style.background = "var(--border-strong)";
    statusEl.textContent = "";
    statusEl.classList.remove("online");
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
      emptyState("📭", "No messages yet", `Say hi to ${state.currentPeer}.`)
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
    el.appendChild(messageEl(m));
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

function messageEl(m) {
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