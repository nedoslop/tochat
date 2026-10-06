import { invoke } from "./api.js";
import { state, resetState, NOTES_PEER, INITIAL_LIMIT, totalUnread, displayName } from "./state.js";
import { toast, dataUrlBytes } from "./utils.js";
import { showConfirm, showPrompt } from "./dialog.js";
import {
    renderSidebar, renderMessages, renderPendingList, showChatView, latestInboundTs,
} from "./ui.js";

// ---------- auth ----------

export async function register() {
    const baseUrl = document.getElementById("base-url").value.trim();
    const username = document.getElementById("user").value.trim();
    const password = document.getElementById("pass").value;
    if (!baseUrl || !username || !password) return toast("Please fill in all fields.");
    try {
        await invoke("register", { baseUrl, username, password });
        toast("Registered. You can sign in now.");
    } catch (e) { toast("Register error: " + e); }
}

export async function login() {
    const baseUrl = document.getElementById("base-url").value.trim();
    const username = document.getElementById("user").value.trim();
    const password = document.getElementById("pass").value;
    if (!baseUrl || !username || !password) return toast("Please fill in all fields.");
    try {
        await invoke("connect", { baseUrl, username, password });
    } catch (e) { toast("Login error: " + e); }
}

export async function logout() {
    try { await invoke("disconnect"); } catch (_) {}
    resetToLogin();
}

export async function reloadUI() {
    try { await invoke("disconnect"); } catch (_) {}
    resetToLogin();
}

export async function deleteAccount() {
    const pw = await showPrompt(
        "Enter your password to delete your account. This cannot be undone.",
        { title: "Delete account", placeholder: "password", okText: "Delete", danger: true },
    );
    if (pw === null) return;
    if (!pw) return toast("Password required.");
    try {
        await invoke("delete_account", { password: pw });
    } catch (e) { toast("Delete error: " + e); }
}

// ---------- chats ----------

export async function openNotes() {
    state.currentPeer = NOTES_PEER;
    state.peers.add(NOTES_PEER);
    state.unread[NOTES_PEER] = 0;
    showChatView();
    try {
        state.msgCache[NOTES_PEER] = await invoke("get_messages", {
            peer: NOTES_PEER, beforeTs: null, limit: null,
        });
    } catch (_) {
        state.msgCache[NOTES_PEER] = state.msgCache[NOTES_PEER] || [];
    }
    renderSidebar();
    renderMessages();
    void updateBadge();
}

export async function openPeer(peer) {
    if (!peer) return;
    if (peer === state.me) return toast("You can't chat with yourself.");
    if (state.blocked.has(peer)) {
        const unblock = await showConfirm(
            `${displayName(peer)} is blocked. Unblock to open the chat?`,
            { title: "Blocked user", okText: "Unblock" },
        );
        if (!unblock) return;
        try { await invoke("unblock_user", { username: peer }); } catch (e) {
            toast("Unblock error: " + e); return;
        }
        state.blocked.delete(peer);
    }

    state.pending.delete(peer);
    state.currentPeer = peer;
    state.peers.add(peer);
    state.unread[peer] = 0;
    showChatView();

    try {
        state.msgCache[peer] = await invoke("get_messages", {
            peer, beforeTs: null, limit: INITIAL_LIMIT,
        });
    } catch (_) {
        state.msgCache[peer] = state.msgCache[peer] || [];
    }

    if (!state.profiles[peer]) {
        try { await invoke("get_profile", { username: peer }); } catch (_) {}
    }

    renderSidebar();
    renderMessages();
    autoPull(peer);
    void sendReadReceipt(peer);
    void updateBadge();
    closeSidebar();
}

export async function acceptPending(peer) {
    state.pending.delete(peer);
    state.peers.add(peer);
    renderSidebar();
    renderPendingList();
    await openPeer(peer);
}

export async function refreshPending() {
    try { await invoke("list_pending"); }
    catch (e) { toast("Refresh pending error: " + e); }
}

function mergeSent(peer, msg) {
    const cache = state.msgCache[peer] || (state.msgCache[peer] = []);
    const idx = cache.findIndex((x) => x.id === msg.id);
    if (idx >= 0) cache[idx] = msg;
    else cache.push(msg);
}

export async function send() {
    if (!state.currentPeer) return;
    const input = document.getElementById("msg");
    const text = input.value;
    if (!text.trim()) return;

    const peer = state.currentPeer;
    try {
        const msg = await invoke("send_message", { to: peer, text });
        input.value = "";
        input.style.height = "auto";
        if (peer !== NOTES_PEER) {
            state.pending.delete(peer);
            state.peers.add(peer);
        }
        mergeSent(peer, msg);
        renderSidebar();
        if (state.currentPeer === peer) renderMessages();
    } catch (e) { toast("Send error: " + e); }
}

export async function sendMedia(kind, payload) {
    const peer = state.currentPeer;
    if (!peer) return;
    try {
        const msg = await invoke("send_media", { to: peer, kind, payload });
        if (peer !== NOTES_PEER) {
            state.pending.delete(peer);
            state.peers.add(peer);
        }
        mergeSent(peer, msg);
        renderSidebar();
        if (state.currentPeer === peer) renderMessages();
    } catch (e) { toast(`Send ${kind} error: ` + e); }
}

async function compressImage(dataUrl, maxDim = 1280, quality = 0.85) {
    try {
        const img = await new Promise((resolve, reject) => {
            const i = new Image();
            i.onload = () => resolve(i);
            i.onerror = () => reject(new Error("invalid image"));
            i.src = dataUrl;
        });

        const needResize = img.width > maxDim || img.height > maxDim;
        const needReencode = dataUrl.length > 400_000;
        if (!needResize && !needReencode) return dataUrl;

        const scale = Math.min(maxDim / img.width, maxDim / img.height, 1);
        const w = Math.max(1, Math.round(img.width * scale));
        const h = Math.max(1, Math.round(img.height * scale));

        const canvas = document.createElement("canvas");
        canvas.width = w;
        canvas.height = h;
        const ctx = canvas.getContext("2d");
        ctx.drawImage(img, 0, 0, w, h);
        const out = canvas.toDataURL("image/jpeg", quality);
        return out.length < dataUrl.length ? out : dataUrl;
    } catch (_) {
        return dataUrl;
    }
}

export async function sendImage(dataUrl) {
    const compressed = await compressImage(dataUrl);
    await sendMedia("image", compressed);
}

export async function sendAudio(dataUrl) { await sendMedia("audio", dataUrl); }

export async function sendFile(file) {
    const dataUrl = await new Promise((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result || ""));
        r.onerror = () => reject(r.error || new Error("read error"));
        r.readAsDataURL(file);
    });
    const payload = JSON.stringify({
        name: file.name || "file",
        size: file.size || dataUrlBytes(dataUrl),
        data: dataUrl,
    });
    await sendMedia("file", payload);
}

export async function editMessage(id) {
    const peer = state.currentPeer;
    if (!peer) return;
    const msg = (state.msgCache[peer] || []).find((m) => m.id === id);
    const currentText = msg ? msg.payload : "";

    const next = await showPrompt("Leave empty to delete the message.", {
        title: "Edit message", defaultValue: currentText, okText: "Save",
    });
    if (next === null) return;

    try {
        const updated = next === ""
            ? await invoke("delete_message", { peer, id })
            : await invoke("edit_message", { peer, id, text: next });
        mergeSent(peer, updated);
        if (state.currentPeer === peer) renderMessages();
    } catch (e) { toast("Edit error: " + e); }
}

export async function deleteMessage(id) {
    const peer = state.currentPeer;
    if (!peer) return;
    const ok = await showConfirm("Delete this message?", {
        title: "Delete message", okText: "Delete", danger: true,
    });
    if (!ok) return;
    try {
        const updated = await invoke("delete_message", { peer, id });
        mergeSent(peer, updated);
        if (state.currentPeer === peer) renderMessages();
    } catch (e) { toast("Delete error: " + e); }
}

// ---------- chat management ----------

export async function clearChat() {
    const peer = state.currentPeer;
    if (!peer) return;
    if (peer === NOTES_PEER) {
        const ok = await showConfirm("Clear all notes on this device?", {
            title: "Clear notes", okText: "Clear", danger: true,
        });
        if (!ok) return;
        try {
            await invoke("clear_chat", { peer: NOTES_PEER });
            state.msgCache[NOTES_PEER] = [];
            state.seenIds[NOTES_PEER] = new Set();
            renderMessages();
        } catch (e) { toast("Clear error: " + e); }
        return;
    }
    const ok = await showConfirm(
        `Delete all local messages with ${displayName(peer)}? The other side keeps their copy.`,
        { title: "Clear chat", okText: "Clear", danger: true },
    );
    if (!ok) return;
    try {
        await invoke("clear_chat", { peer });
        state.msgCache[peer] = [];
        state.seenIds[peer] = new Set();
        state.mightHaveMore[peer] = undefined;
        renderMessages();
    } catch (e) { toast("Clear error: " + e); }
}

export async function leaveChat() {
    const peer = state.currentPeer;
    if (!peer || peer === NOTES_PEER) return;
    const ok = await showConfirm(
        `Leave the chat with ${displayName(peer)}? Both sides will lose the relationship and local history will be deleted on your side.`,
        { title: "Leave chat", okText: "Leave", danger: true },
    );
    if (!ok) return;
    try { await invoke("leave_chat", { peer }); }
    catch (e) { toast("Leave error: " + e); }
}

export async function blockUser() {
    const peer = state.currentPeer;
    if (!peer || peer === NOTES_PEER) return;
    const ok = await showConfirm(
        `Block ${displayName(peer)}? They won't be able to send you messages.`,
        { title: "Block user", okText: "Block", danger: true },
    );
    if (!ok) return;
    try {
        await invoke("block_user", { username: peer });
        state.blocked.add(peer);
        state.peers.delete(peer);
        state.pending.delete(peer);
        state.currentPeer = null;
        delete state.msgCache[peer];
        delete state.seenIds[peer];
        renderSidebar();
        renderMessages();
    } catch (e) { toast("Block error: " + e); }
}

// ---------- read receipts ----------

export async function sendReadReceipt(peer) {
    if (!peer || peer === NOTES_PEER) return;
    if (!state.me) return;
    const upTo = latestInboundTs(peer);
    if (upTo <= 0) return;
    try {
        await invoke("mark_read", { peer, upToTs: upTo });
        for (const m of state.msgCache[peer] || []) {
            if (m.direction === "in") m.read = true;
        }
        state.unread[peer] = 0;
        renderSidebar();
        void updateBadge();
    } catch (_) {}
}

export async function refreshReadState(peer) {
    if (!peer || peer === NOTES_PEER) return;
    try {
        const latest = await invoke("get_messages", {
            peer, beforeTs: null, limit: INITIAL_LIMIT,
        });
        // Merge, don't replace — otherwise we'd drop older pages loaded
        // via scroll-back pagination.
        const existing = state.msgCache[peer] || [];
        const byId = new Map();
        for (const m of existing) byId.set(m.id, m);
        for (const m of latest) byId.set(m.id, m);
        state.msgCache[peer] = [...byId.values()].sort(
            (a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts
        );
        if (state.currentPeer === peer) renderMessages();
    } catch (_) {}
}

// ---------- history sync ----------

/**
 * Ask the peer for their newest page of messages.
 *
 * No in-flight guard: the previous guard was the cause of the
 * "peer never syncs" bug — the first attempt (fired right after the
 * peer's PeerOnline arrived, potentially before their session was fully
 * registered server-side) would fail, and every retry within the next
 * 6 s was silently dropped because `state.pulling` still contained the
 * peer. Duplicate pulls are safe: DB upsert is keyed on message id.
 *
 * Always uses `since: 0` (not a timestamp from our local cache). The
 * cache is a strict subset of the peer's history, so asking "give me
 * everything" and relying on id-keyed upsert to dedupe is both simpler
 * and correct — including the offline→online flip where one side has
 * a message the other has never seen.
 */
export async function autoPull(peer) {
    if (!peer || peer === state.me || peer === NOTES_PEER) return;
    try {
        // Cheap local warm-up: don't await network, just make sure the
        // cache exists so the UI shows something while the sync flies.
        if (!state.msgCache[peer]) {
            state.msgCache[peer] = await invoke("get_messages", {
                peer, beforeTs: null, limit: INITIAL_LIMIT,
            });
        }
        await invoke("pull_history", {
            from: peer, since: 0, limit: INITIAL_LIMIT, before: null,
        });
    } catch (e) {
        console.warn("[autoPull] failed for", peer, e);
    }
}

export async function loadOlder() {
    const peer = state.currentPeer;
    if (!peer || peer === NOTES_PEER) return;
    if (state.loadingOlder.has(peer)) return;
    if (state.mightHaveMore[peer] === false) return;

    const msgs = state.msgCache[peer] || [];
    if (msgs.length === 0) return;

    const oldestTs = msgs.reduce((min, m) => (m.ts < min ? m.ts : min), msgs[0].ts);

    state.loadingOlder.add(peer);
    state.suppressScrollLoad = true;

    try {
        const older = await invoke("get_messages", {
            peer, beforeTs: oldestTs, limit: INITIAL_LIMIT,
        });
        if (older.length > 0) {
            const existing = new Set((state.msgCache[peer] || []).map((m) => m.id));
            const merged = [...older.filter((m) => !existing.has(m.id)), ...(state.msgCache[peer] || [])];
            merged.sort((a, b) => a.ts - b.ts || a.edit_ts - b.edit_ts);
            state.msgCache[peer] = merged;
            renderMessages(true);
            requestAnimationFrame(() => { state.suppressScrollLoad = false; });
            state.loadingOlder.delete(peer);
            return;
        }

        state.lastPullLimit[peer] = INITIAL_LIMIT;
        await invoke("pull_history", {
            from: peer, since: 0, limit: INITIAL_LIMIT, before: oldestTs,
        });
    } catch (e) {
        state.loadingOlder.delete(peer);
        delete state.lastPullLimit[peer];
        state.suppressScrollLoad = false;
        toast("Load older error: " + e);
        renderMessages();
    }
}

export function lastReceivedEditTs(peer) {
    const msgs = state.msgCache[peer] || [];
    let max = 0;
    for (const m of msgs) if (m.direction === "in" && m.edit_ts > max) max = m.edit_ts;
    return max;
}

// ---------- badge ----------

export async function updateBadge() {
    try {
        await invoke("update_badge", { count: totalUnread() });
    } catch (_) {}
}

// ---------- sidebar (mobile) ----------

export function openSidebar() {
    document.getElementById("app-view").classList.add("sidebar-open");
    document.getElementById("sidebar-backdrop").hidden = false;
}
export function closeSidebar() {
    document.getElementById("app-view").classList.remove("sidebar-open");
    document.getElementById("sidebar-backdrop").hidden = true;
}

// ---------- navigation ----------

export function resetToLogin() {
    resetState();
    document.getElementById("app-view").hidden = true;
    document.getElementById("login-view").hidden = false;
    document.getElementById("enc-panel").hidden = true;
    document.getElementById("settings-menu").hidden = true;
    document.getElementById("chat-menu").hidden = true;
    closeSidebar();
    renderSidebar();
    renderMessages();
    void updateBadge();
}
