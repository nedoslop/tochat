import { invoke, listen } from "./api.js";
import { state, NOTES_PEER, INITIAL_LIMIT, totalUnread } from "./state.js";
import { toast, avatarColor, initial } from "./utils.js";
import { showAlert } from "./dialog.js";
import {
    renderSidebar, renderMessages, renderPendingList, renderMyStatus,
    showChatView, updateReadIndicators,
} from "./ui.js";
import {
    autoPull, resetToLogin, sendReadReceipt, refreshReadState, openPeer, updateBadge,
} from "./actions.js";
import { refreshEncryptionStatus } from "./encryption.js";

function applyMyAvatar() {
    const av = document.getElementById("me-avatar");
    const mine = state.profiles[state.me];
    if (mine && mine.avatar) {
        av.innerHTML = "";
        const img = document.createElement("img");
        img.src = mine.avatar;
        img.alt = "";
        img.className = "avatar-img";
        av.appendChild(img);
        av.style.background = "var(--surface-3)";
    } else {
        av.textContent = initial(state.me);
        av.style.background = avatarColor(state.me);
    }
}

function mergeIntoCache(peer, incoming) {
    const existing = state.msgCache[peer] || [];
    const byId = new Map();
    for (const m of existing) byId.set(m.id, m);
    for (const m of incoming) byId.set(m.id, m);
    const merged = [...byId.values()].sort(
        (a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts
    );
    state.msgCache[peer] = merged;
}

/**
 * Trigger a peer sync. The peer may not be reachable yet (their session
 * is registered on the server slightly after PeerOnline is emitted), so
 * we schedule a couple of retries. Each call is a full "give me your
 * newest page" pull; the DB upsert is id-keyed so overlaps are cheap.
 */
function syncWithPeer(peer, { delay = 0 } = {}) {
    if (!peer || peer === state.me || peer === NOTES_PEER) return;
    const run = () => { autoPull(peer); };
    if (delay > 0) setTimeout(run, delay);
    else run();
}

export async function setupEvents() {
    await listen("auth-ok", async (e) => {
        state.me = e.payload.username;

        document.getElementById("me-name").textContent = state.me;
        const av = document.getElementById("me-avatar");
        av.textContent = initial(state.me);
        av.style.background = avatarColor(state.me);

        document.getElementById("login-view").hidden = true;
        document.getElementById("app-view").hidden = false;

        try {
            const counts = await invoke("get_unread_counts");
            for (const [peer, n] of counts) state.unread[peer] = n;
        } catch (_) {}

        renderMyStatus();
        showChatView();
        renderSidebar();
        renderMessages();
        void updateBadge();
    });

    await listen("peers", (e) => {
        for (const p of e.payload) state.peers.add(p);
        renderSidebar();
        (async () => {
            for (const p of e.payload) {
                try {
                    const msgs = await invoke("get_messages", {
                        peer: p, beforeTs: null, limit: INITIAL_LIMIT,
                    });
                    mergeIntoCache(p, msgs);
                } catch (_) {}
                if (!state.profiles[p]) {
                    invoke("get_profile", { username: p }).catch(() => {});
                }
                // Fire a sync for every peer in the list. If the peer is
                // currently offline, the server's "peer is offline" error
                // is suppressed by the error handler below, and PeerOnline
                // will trigger a retry when they come back.
                syncWithPeer(p);
            }
            renderSidebar();
        })();
    });

    await listen("pending-chats", (e) => {
        state.pending.clear();
        for (const p of e.payload) {
            if (!state.peers.has(p)) state.pending.add(p);
            if (!state.profiles[p]) {
                invoke("get_profile", { username: p }).catch(() => {});
            }
        }
        renderSidebar();
        renderPendingList();
    });

    await listen("blocked", (e) => {
        state.blocked.clear();
        for (const p of e.payload) state.blocked.add(p);
        renderSidebar();
    });

    await listen("profile", (e) => {
        const { username, display_name, avatar } = e.payload;
        if (!username) return;
        state.profiles[username] = { display_name, avatar };
        if (username === state.me) applyMyAvatar();
        renderSidebar();
    });

    // ---------------------------------------------------------------------
    // peer-online: fires when a peer (re)connects. This is the ONLY
    // reliable trigger for history sync in the offline→online case.
    //
    // The server now emits this on EVERY new session (not just the first
    // one), so a stale session entry from a silent disconnect can no
    // longer suppress the notification.
    //
    // We schedule three attempts (0 ms, 500 ms, 2500 ms) to cover the
    // race window in which the server has registered the peer's session
    // but the peer's own reader loop isn't draining yet.
    // ---------------------------------------------------------------------
    await listen("peer-online", (e) => {
        const name = e.payload;
        state.online.add(name);
        state.peerStatus[name] = state.peerStatus[name] || "online";
        state.peers.add(name);
        renderSidebar();

        syncWithPeer(name);
        syncWithPeer(name, { delay: 500 });
        syncWithPeer(name, { delay: 2500 });

        if (state.currentPeer === name) {
            refreshReadState(name).then(() => {
                if (state.currentPeer === name) void sendReadReceipt(name);
            });
        }
        if (!state.profiles[name]) {
            invoke("get_profile", { username: name }).catch(() => {});
        }
    });

    await listen("peer-offline", (e) => {
        state.online.delete(e.payload);
        delete state.peerStatus[e.payload];
        renderSidebar();
    });

    await listen("status-update", (e) => {
        const { username, status } = e.payload;
        state.peerStatus[username] = status;
        if (status === "invisible") state.online.delete(username);
        else state.online.add(username);
        renderSidebar();
    });

    await listen("chat-left", (e) => {
        const peer = e.payload;
        state.peers.delete(peer);
        state.pending.delete(peer);
        delete state.msgCache[peer];
        delete state.seenIds[peer];
        delete state.mightHaveMore[peer];
        delete state.lastPullLimit[peer];
        state.unread[peer] = 0;
        if (state.currentPeer === peer) { state.currentPeer = null; renderMessages(); }
        renderSidebar();
        void updateBadge();
        toast(`Chat with ${peer} was closed.`);
    });

    await listen("message", (e) => {
        const m = e.payload;
        state.pending.delete(m.peer);
        state.peers.add(m.peer);

        if (!state.msgCache[m.peer]) state.msgCache[m.peer] = [];
        const idx = state.msgCache[m.peer].findIndex((x) => x.id === m.id);

        const entry = {
            id: m.id, direction: m.direction, ts: m.ts, edit_ts: m.edit_ts,
            kind: m.kind, payload: m.payload, read: m.direction === "out",
        };

        if (idx >= 0) state.msgCache[m.peer][idx] = entry;
        else state.msgCache[m.peer].push(entry);

        const isInbound = m.direction === "in";
        const isOpen = state.currentPeer === m.peer;

        if (isInbound && !isOpen) {
            state.unread[m.peer] = (state.unread[m.peer] || 0) + 1;
            void updateBadge();
        }

        renderSidebar();
        renderPendingList();
        if (isOpen) {
            renderMessages();
            void sendReadReceipt(m.peer);
        }
    });

    await listen("note-message", async (e) => {
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
        const peer = e.payload;

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
            const map = Object.fromEntries(counts);
            state.unread[peer] = map[peer] || 0;
            void updateBadge();
        } catch (_) {}

        renderSidebar();

        if (state.currentPeer === peer) {
            renderMessages(wasLoadingOlder);
            if (wasLoadingOlder) {
                requestAnimationFrame(() => { state.suppressScrollLoad = false; });
            } else {
                void sendReadReceipt(peer);
            }
        } else {
            state.suppressScrollLoad = false;
        }
    });

    await listen("error", (e) => {
        const msg = String(e.payload);
        const m = msg.match(/^peer (.+?) is offline/);
        if (m) {
            // Swallow. This is an expected condition during the
            // offline→online handshake window; PeerOnline will retrigger.
            state.pulling.delete(m[1]);
            state.loadingOlder.delete(m[1]);
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
            await showAlert("You were signed in from another window or device. This session has been closed.", { title: "Session closed" });
        } else {
            await showAlert("The server closed this session.", { title: "Session closed" });
        }
        try { await invoke("disconnect"); } catch (_) {}
        resetToLogin();
    });

    await listen("disconnected", () => {
        if (state.me) resetToLogin();
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
