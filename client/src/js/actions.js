// User-initiated actions.

import { invoke } from "./api.js";
import { state, resetState, NOTES_PEER, INITIAL_LIMIT } from "./state.js";
import { toast } from "./utils.js";
import { showConfirm, showPrompt } from "./dialog.js";
import {
    renderSidebar,
    renderMessages,
    renderPendingList,
    showChatView,
    latestInboundTs,
} from "./ui.js";

// ---------- auth ----------

export async function register() {
    const baseUrl = document.getElementById("base-url").value.trim();
    const username = document.getElementById("user").value.trim();
    const password = document.getElementById("pass").value;
    if (!baseUrl || !username || !password) {
        return toast("Please fill in all fields.");
    }
    try {
        await invoke("register", { baseUrl, username, password });
        toast("Registered. You can sign in now.");
    } catch (e) {
        toast("Register error: " + e);
    }
}

export async function login() {
    const baseUrl = document.getElementById("base-url").value.trim();
    const username = document.getElementById("user").value.trim();
    const password = document.getElementById("pass").value;
    if (!baseUrl || !username || !password) {
        return toast("Please fill in all fields.");
    }
    try {
        await invoke("connect", { baseUrl, username, password });
    } catch (e) {
        toast("Login error: " + e);
    }
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
        {
            title: "Delete account",
            placeholder: "password",
            okText: "Delete",
            danger: true,
        },
    );
    if (pw === null) return;
    if (!pw) return toast("Password required.");
    try {
        await invoke("delete_account", { password: pw });
    } catch (e) {
        toast("Delete error: " + e);
    }
}

// ---------- chats ----------

export async function openNotes() {
    state.currentPeer = NOTES_PEER;
    state.peers.add(NOTES_PEER);
    state.unread[NOTES_PEER] = 0;
    showChatView();
    try {
        state.msgCache[NOTES_PEER] = await invoke("get_messages", { peer: NOTES_PEER });
    } catch (_) {
        state.msgCache[NOTES_PEER] = state.msgCache[NOTES_PEER] || [];
    }
    renderSidebar();
    renderMessages();
}

export async function openPeer(peer) {
    if (!peer) return;
    if (peer === state.me) return toast("You can't chat with yourself.");
    if (state.blocked.has(peer)) {
        const unblock = await showConfirm(
            `${peer} is blocked. Unblock to open the chat?`,
            { title: "Blocked user", okText: "Unblock" },
        );
        if (!unblock) return;
        try { await invoke("unblock_user", { username: peer }); } catch (e) {
            toast("Unblock error: " + e);
            return;
        }
        state.blocked.delete(peer);
    }

    state.pending.delete(peer);
    state.currentPeer = peer;
    state.peers.add(peer);
    state.unread[peer] = 0;
    showChatView();

    try {
        state.msgCache[peer] = await invoke("get_messages", { peer });
    } catch (_) {
        state.msgCache[peer] = state.msgCache[peer] || [];
    }

    renderSidebar();
    renderMessages();
    autoPull(peer);
    void sendReadReceipt(peer);
}

export async function acceptPending(peer) {
    state.pending.delete(peer);
    state.peers.add(peer);
    renderSidebar();
    renderPendingList();
    await openPeer(peer);
}

export async function refreshPending() {
    try {
        await invoke("list_pending");
    } catch (e) {
        toast("Refresh pending error: " + e);
    }
}

export async function send() {
    if (!state.currentPeer) return;
    const input = document.getElementById("msg");
    const text = input.value;
    if (!text.trim()) return;

    try {
        await invoke("send_message", { to: state.currentPeer, text });
        input.value = "";
        input.style.height = "auto";
        if (state.currentPeer !== NOTES_PEER) state.pending.delete(state.currentPeer);
        state.msgCache[state.currentPeer] = await invoke("get_messages", {
            peer: state.currentPeer,
        });
        renderSidebar();
        renderMessages();
    } catch (e) {
        toast("Send error: " + e);
    }
}

/**
 * Generic media send. `kind` is "image" | "audio" | "file".
 * `payload` must be a data URL.
 */
export async function sendMedia(kind, payload) {
    if (!state.currentPeer) return;
    try {
        await invoke("send_media", {
            to: state.currentPeer,
            kind,
            payload,
        });
        if (state.currentPeer !== NOTES_PEER) state.pending.delete(state.currentPeer);
        state.msgCache[state.currentPeer] = await invoke("get_messages", {
            peer: state.currentPeer,
        });
        renderSidebar();
        renderMessages();
    } catch (e) {
        toast(`Send ${kind} error: ` + e);
    }
}

export async function sendImage(dataUrl) {
    await sendMedia("image", dataUrl);
}

export async function sendAudio(dataUrl) {
    await sendMedia("audio", dataUrl);
}

export async function editMessage(id) {
    const peer = state.currentPeer;
    if (!peer) return;

    const msg = (state.msgCache[peer] || []).find((m) => m.id === id);
    const currentText = msg ? msg.payload : "";

    const next = await showPrompt(
        "Leave empty to delete the message.",
        {
            title: "Edit message",
            defaultValue: currentText,
            okText: "Save",
        },
    );
    if (next === null) return;

    try {
        if (next === "") {
            await invoke("delete_message", { peer, id });
        } else {
            await invoke("edit_message", { peer, id, text: next });
        }
        state.msgCache[peer] = await invoke("get_messages", { peer });
        renderMessages();
    } catch (e) {
        toast("Edit error: " + e);
    }
}

export async function deleteMessage(id) {
    const peer = state.currentPeer;
    if (!peer) return;

    const ok = await showConfirm("Delete this message?", {
        title: "Delete message",
        okText: "Delete",
        danger: true,
    });
    if (!ok) return;

    try {
        await invoke("delete_message", { peer, id });
        state.msgCache[peer] = await invoke("get_messages", { peer });
        renderMessages();
    } catch (e) {
        toast("Delete error: " + e);
    }
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
            renderMessages();
        } catch (e) { toast("Clear error: " + e); }
        return;
    }
    const ok = await showConfirm(
        `Delete all local messages with ${peer}? The other side keeps their copy.`,
        { title: "Clear chat", okText: "Clear", danger: true },
    );
    if (!ok) return;
    try {
        await invoke("clear_chat", { peer });
        state.msgCache[peer] = [];
        renderMessages();
    } catch (e) {
        toast("Clear error: " + e);
    }
}

export async function leaveChat() {
    const peer = state.currentPeer;
    if (!peer || peer === NOTES_PEER) return;
    const ok = await showConfirm(
        `Leave the chat with ${peer}? Both sides will lose the relationship and local history will be deleted on your side. You can start a new chat later.`,
        { title: "Leave chat", okText: "Leave", danger: true },
    );
    if (!ok) return;
    try {
        await invoke("leave_chat", { peer });
    } catch (e) {
        toast("Leave error: " + e);
    }
}

export async function blockUser() {
    const peer = state.currentPeer;
    if (!peer || peer === NOTES_PEER) return;
    const ok = await showConfirm(
        `Block ${peer}? They won't be able to send you messages. You can unblock later by trying to open the chat.`,
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
        renderSidebar();
        renderMessages();
    } catch (e) {
        toast("Block error: " + e);
    }
}

// ---------- read receipts ----------

export async function sendReadReceipt(peer) {
    if (!peer || peer === NOTES_PEER) return;
    if (!state.me) return;
    const upTo = latestInboundTs(peer);
    if (upTo <= 0) return;
    try {
        await invoke("mark_read", { peer, upToTs: upTo });
    } catch (_) {
        /* best effort */
    }
}

export async function refreshReadState(peer) {
    if (!peer || peer === NOTES_PEER) return;
    try {
        state.msgCache[peer] = await invoke("get_messages", { peer });
        if (state.currentPeer === peer) renderMessages();
    } catch (_) {}
}

// ---------- history sync ----------

export async function autoPull(peer) {
    if (!peer || peer === state.me || peer === NOTES_PEER) return;
    if (state.pulling.has(peer)) return;

    state.pulling.add(peer);
    try {
        if (!state.msgCache[peer]) {
            state.msgCache[peer] = await invoke("get_messages", { peer });
        }
        const localMsgs = state.msgCache[peer];

        if (localMsgs.length === 0) {
            state.lastPullLimit[peer] = INITIAL_LIMIT;
            await invoke("pull_history", {
                from: peer,
                since: 0,
                limit: INITIAL_LIMIT,
                before: null,
            });
        } else {
            const since = lastReceivedEditTs(peer);
            state.lastPullLimit[peer] = null;
            await invoke("pull_history", {
                from: peer,
                since,
                limit: null,
                before: null,
            });
        }
    } catch (_) {
        state.pulling.delete(peer);
        return;
    }
    setTimeout(() => state.pulling.delete(peer), 6000);
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
    state.lastPullLimit[peer] = INITIAL_LIMIT;
    state.suppressScrollLoad = true;

    try {
        await invoke("pull_history", {
            from: peer,
            since: 0,
            limit: INITIAL_LIMIT,
            before: oldestTs,
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
    for (const m of msgs) {
        if (m.direction === "in" && m.edit_ts > max) max = m.edit_ts;
    }
    return max;
}

// ---------- navigation ----------

export function resetToLogin() {
    resetState();
    document.getElementById("app-view").hidden = true;
    document.getElementById("login-view").hidden = false;
    document.getElementById("enc-panel").hidden = true;
    document.getElementById("settings-menu").hidden = true;
    document.getElementById("chat-menu").hidden = true;
    renderSidebar();
    renderMessages();
}