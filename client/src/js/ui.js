import { state, NOTES_PEER, displayName, avatarFor } from "./state.js";
import {
    avatarColor, initial, formatTime, formatDate, formatBytes, parseFilePayload,
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

function renderAvatar(el, username, fallbackText) {
    const img = avatarFor(username);
    el.innerHTML = "";
    if (img) {
        const i = document.createElement("img");
        i.src = img;
        i.alt = "";
        i.className = "avatar-img";
        el.appendChild(i);
        el.style.background = "var(--surface-3)";
        el.textContent = "";
    } else {
        el.textContent = fallbackText;
    }
}

export function renderSidebar() {
    const peersEl = document.getElementById("peers");
    peersEl.innerHTML = "";

    const all = new Set([...state.peers, ...state.pending]);
    if (state.me) all.add(NOTES_PEER);

    if (all.size === 0) {
        const empty = document.createElement("div");
        empty.className = "hint";
        empty.style.padding = "8px 6px";
        empty.textContent = "No chats yet.";
        peersEl.appendChild(empty);
    } else {
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
            if (isNotes) {
                av.textContent = "📝";
                av.style.background = "#8b5cf6";
            } else if (avatarFor(p)) {
                av.style.background = "var(--surface-3)";
                renderAvatar(av, p, initial(p));
            } else {
                av.style.background = avatarColor(p);
                av.textContent = initial(p);
            }

            const body = document.createElement("div");
            body.className = "peer-body";
            const name = document.createElement("div");
            name.className = "peer-name";
            name.textContent = isNotes ? "Notes" : displayName(p);
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

    const titleEl = document.getElementById("peer-title");
    const statusEl = document.getElementById("peer-status");
    const peerAvatar = document.getElementById("peer-avatar");

    if (state.currentPeer) {
        const isNotes = state.currentPeer === NOTES_PEER;
        titleEl.textContent = isNotes ? "Notes" : displayName(state.currentPeer);
        if (isNotes) {
            peerAvatar.innerHTML = "📝";
            peerAvatar.style.background = "#8b5cf6";
        } else if (avatarFor(state.currentPeer)) {
            peerAvatar.style.background = "var(--surface-3)";
            renderAvatar(peerAvatar, state.currentPeer, initial(state.currentPeer));
        } else {
            peerAvatar.style.background = avatarColor(state.currentPeer);
            peerAvatar.textContent = initial(state.currentPeer);
        }
        if (isNotes) {
            statusEl.textContent = "synced across your devices";
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

export function renderMessages(preserveScroll = false) {
    const el = document.getElementById("messages");
    const prevHeight = el.scrollHeight;
    const prevTop = el.scrollTop;
    const prevClientHeight = el.clientHeight;
    const wasAtBottom = prevHeight - prevTop - prevClientHeight < 60;

    el.innerHTML = "";

    if (!state.currentPeer) {
        el.appendChild(emptyState("💬", "No chat selected", "Pick a chat from the sidebar or start a new one."));
        return;
    }

    const peer = state.currentPeer;
    const msgs = (state.msgCache[peer] || []).slice().sort((a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts);

    if (msgs.length === 0) {
        el.appendChild(emptyState(
            peer === NOTES_PEER ? "📝" : "📭",
            peer === NOTES_PEER ? "No notes yet" : "No messages yet",
            peer === NOTES_PEER
                ? "Anything you type here stays on this device — and syncs across your sessions."
                : `Say hi to ${displayName(peer)}.`,
        ));
        return;
    }

    const seen = state.seenIds[peer] || (state.seenIds[peer] = new Set());

    if (peer !== NOTES_PEER) {
        const loading = state.loadingOlder.has(peer);
        const reachedStart = state.mightHaveMore[peer] === false;
        if (loading) {
            const b = document.createElement("div");
            b.className = "load-older";
            b.textContent = "Loading…";
            el.appendChild(b);
        } else if (reachedStart) {
            const b = document.createElement("div");
            b.className = "load-older-note";
            b.textContent = "— start of conversation —";
            el.appendChild(b);
        } else {
            const b = document.createElement("div");
            b.className = "load-older";
            b.textContent = "Scroll up to load older messages";
            el.appendChild(b);
        }
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
        const isFresh = !seen.has(m.id);
        seen.add(m.id);
        el.appendChild(messageEl(m, peer === NOTES_PEER, isFresh));
    }

    if (preserveScroll) {
        const newHeight = el.scrollHeight;
        el.scrollTop = newHeight - prevHeight + prevTop;
    } else if (wasAtBottom) {
        el.scrollTop = el.scrollHeight;
    }

    if (seen.size > msgs.length * 2 + 200) {
        const live = new Set(msgs.map((m) => m.id));
        state.seenIds[peer] = live;
    }
}

export function updateReadIndicators(peer) {
    if (state.currentPeer !== peer) return;
    const el = document.getElementById("messages");
    if (!el) return;
    for (const m of state.msgCache[peer] || []) {
        if (m.direction !== "out" || m.payload === "") continue;
        const node = el.querySelector(`.msg[data-id="${cssEscape(m.id)}"] .msg-time`);
        if (!node) continue;
        const label = formatTime(m.ts) + (m.read ? "  ✓✓" : "  ✓");
        if (node.textContent !== label) node.textContent = label;
    }
}

function cssEscape(s) {
    if (window.CSS && typeof window.CSS.escape === "function") return window.CSS.escape(s);
    return String(s).replace(/["\\]/g, "\\$&");
}

function emptyState(icon, title, sub) {
    const d = document.createElement("div");
    d.className = "empty-state";
    const i = document.createElement("div"); i.className = "empty-icon"; i.textContent = icon;
    const t = document.createElement("div"); t.className = "empty-title"; t.textContent = title;
    const s = document.createElement("div"); s.className = "empty-sub"; s.textContent = sub;
    d.appendChild(i); d.appendChild(t); d.appendChild(s);
    return d;
}

function messageEl(m, isNotes, isFresh = false) {
    const div = document.createElement("div");
    const deleted = m.payload === "";
    div.className =
        "msg " + (m.direction === "out" ? "out" : "in") +
        (deleted ? " deleted" : "") +
        (m.kind && m.kind !== "text" ? " kind-" + m.kind : "") +
        (isFresh ? " fresh" : "");
    div.dataset.id = m.id;

    const body = document.createElement("div");
    body.className = "msg-body";

    if (deleted) {
        body.textContent = "(deleted)";
    } else if (m.kind === "image") {
        // Encryption mismatch produces a payload that isn't a data URL;
        // show a clear placeholder instead of a broken image icon.
        if (!/^data:image\//i.test(m.payload)) {
            const err = document.createElement("div");
            err.className = "msg-image-error";
            err.textContent = "🔒 Cannot display image — encryption key mismatch?";
            body.appendChild(err);
        } else {
            const img = document.createElement("img");
            img.className = "msg-image";
            img.src = m.payload;
            img.alt = "image";
            img.loading = "lazy";
            img.addEventListener("error", () => {
                body.innerHTML = "";
                const err = document.createElement("div");
                err.className = "msg-image-error";
                err.textContent = "⚠️ Image failed to load.";
                body.appendChild(err);
            });
            img.addEventListener("click", () => openImageViewer(m.payload));
            body.appendChild(img);
        }
    } else if (m.kind === "audio") {
        const wrap = document.createElement("div");
        wrap.className = "msg-audio-wrap";
        const audio = document.createElement("audio");
        audio.className = "msg-audio";
        audio.controls = true;
        audio.preload = "metadata";
        audio.src = m.payload;
        wrap.appendChild(audio);
        body.appendChild(wrap);
    } else if (m.kind === "file") {
        const info = parseFilePayload(m.payload);
        const a = document.createElement("a");
        a.className = "msg-file";
        a.href = info.data;
        a.download = info.name || "file";
        a.rel = "noopener";
        a.title = "Download " + (info.name || "file");

        const icon = document.createElement("span");
        icon.className = "file-icon";
        icon.textContent = "📎";

        const nameEl = document.createElement("span");
        nameEl.className = "file-name";
        nameEl.textContent = info.name || "file";

        a.appendChild(icon);
        a.appendChild(nameEl);
        if (info.size) {
            const sizeEl = document.createElement("span");
            sizeEl.className = "file-size";
            sizeEl.textContent = formatBytes(info.size);
            a.appendChild(sizeEl);
        }
        body.appendChild(a);
    } else {
        body.textContent = m.payload;
    }

    const time = document.createElement("div");
    time.className = "msg-time";
    let label = formatTime(m.ts);
    if (m.direction === "out" && !deleted) label += m.read ? "  ✓✓" : "  ✓";
    time.textContent = label;

    div.appendChild(body);
    div.appendChild(time);

    if (m.direction === "out" && !deleted) {
        const actions = document.createElement("div");
        actions.className = "msg-actions";

        if (m.kind === "text" || !m.kind) {
            const editBtn = document.createElement("button");
            editBtn.dataset.act = "edit";
            editBtn.title = "Edit";
            editBtn.textContent = "✏️";
            actions.appendChild(editBtn);
        }

        const delBtn = document.createElement("button");
        delBtn.dataset.act = "del";
        delBtn.title = "Delete";
        delBtn.textContent = "🗑";
        actions.appendChild(delBtn);

        div.appendChild(actions);
    }

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
        av.style.position = "relative";
        if (avatarFor(p)) {
            av.style.background = "var(--surface-3)";
            renderAvatar(av, p, initial(p));
        } else {
            av.style.background = avatarColor(p);
            av.textContent = initial(p);
        }

        const name = document.createElement("span");
        name.className = "name";
        name.textContent = displayName(p);

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

export function latestInboundTs(peer) {
    const msgs = state.msgCache[peer] || [];
    let max = 0;
    for (const m of msgs) if (m.direction === "in" && m.ts > max) max = m.ts;
    return max;
}

// ---------- Image viewer ----------

export function openImageViewer(src) {
    const v = document.getElementById("image-viewer");
    const img = document.getElementById("image-viewer-img");
    img.src = src;
    v.hidden = false;
}

export function closeImageViewer() {
    const v = document.getElementById("image-viewer");
    const img = document.getElementById("image-viewer-img");
    img.src = "";
    v.hidden = true;
}