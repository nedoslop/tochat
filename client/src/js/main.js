// Entry point.

import { invoke } from "./api.js";
import { state, NOTES_PEER } from "./state.js";
import { toast } from "./utils.js";
import {
    register,
    login,
    send,
    logout,
    reloadUI,
    deleteAccount,
    openPeer,
    openNotes,
    editMessage,
    deleteMessage,
    acceptPending,
    refreshPending,
    clearChat,
    leaveChat,
    blockUser,
    loadOlder,
    sendImage,
    sendAudio,
    sendMedia,
} from "./actions.js";
import {
    renderMessages,
    renderMyStatus,
    showChatView,
    showPendingView,
} from "./ui.js";
import {
    refreshEncryptionStatus,
    openEncPanel,
    closeEncPanel,
    applyEncryption,
    generatePsk,
} from "./encryption.js";
import { initTheme, cycleTheme } from "./theme.js";
import { setupEvents } from "./events.js";

async function installContextMenuGuard() {
    try {
        const isRelease = await invoke("is_release");
        if (isRelease) {
            document.addEventListener("contextmenu", (e) => e.preventDefault());
        }
    } catch (_) {}
}

function installShortcutGuard() {
    const block = (e) => {
        const k = e.key;
        const ctrl = e.ctrlKey || e.metaKey;
        if (
            k === "F5" ||
            k === "F12" ||
            (ctrl && !e.shiftKey && k.toLowerCase() === "r") ||
            (ctrl && e.shiftKey && k.toLowerCase() === "r") ||
            (ctrl && k.toLowerCase() === "u") ||
            (ctrl && k.toLowerCase() === "p") ||
            (ctrl && e.shiftKey && k.toLowerCase() === "p") ||
            (ctrl && e.shiftKey && ["i", "c", "j"].includes(k.toLowerCase())) ||
            (ctrl && k.toLowerCase() === "n") ||
            (ctrl && k.toLowerCase() === "t") ||
            (ctrl && k.toLowerCase() === "w") ||
            (ctrl && e.shiftKey && k.toLowerCase() === "w")
        ) {
            e.preventDefault();
            e.stopPropagation();
        }
    };
    document.addEventListener("keydown", block, true);
}

function closeAllMenus() {
    for (const id of ["settings-menu", "chat-menu"]) {
        const el = document.getElementById(id);
        if (el) el.hidden = true;
    }
}

const MAX_IMAGE_BYTES = 8 * 1024 * 1024;
const MAX_AUDIO_BYTES = 16 * 1024 * 1024;
const MAX_FILE_BYTES = 16 * 1024 * 1024;

function readAsDataUrl(file) {
    return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => resolve(String(reader.result || ""));
        reader.onerror = () => reject(reader.error || new Error("read error"));
        reader.readAsDataURL(file);
    });
}

function kindForFile(file) {
    if (file.type.startsWith("image/")) return "image";
    if (file.type.startsWith("audio/")) return "audio";
    return "file";
}

function limitForKind(kind) {
    if (kind === "image") return MAX_IMAGE_BYTES;
    if (kind === "audio") return MAX_AUDIO_BYTES;
    return MAX_FILE_BYTES;
}

async function dispatchFiles(files) {
    for (const file of files) {
        const kind = kindForFile(file);
        const limit = limitForKind(kind);
        if (file.size > limit) {
            toast(`File too large (max ${Math.round(limit / (1024 * 1024))} MiB).`);
            continue;
        }
        try {
            const dataUrl = await readAsDataUrl(file);
            if (kind === "image") await sendImage(dataUrl);
            else if (kind === "audio") await sendAudio(dataUrl);
            else await sendMedia("file", dataUrl);
        } catch (e) {
            toast("Send file error: " + e);
        }
    }
}

function wireFilePicker({ buttonId, inputId, kind, onPayload }) {
    const btn = document.getElementById(buttonId);
    const input = document.getElementById(inputId);
    if (!btn || !input) return;

    btn.addEventListener("click", () => {
        input.value = "";
        input.click();
    });

    input.addEventListener("change", async () => {
        const files = input.files;
        if (!files || files.length === 0) return;
        const file = files[0];
        const limit = limitForKind(kind);
        if (file.size > limit) {
            toast(`File too large (max ${Math.round(limit / (1024 * 1024))} MiB).`);
            return;
        }
        try {
            const dataUrl = await readAsDataUrl(file);
            if (!dataUrl.startsWith("data:")) return;
            onPayload(dataUrl);
        } catch (e) {
            toast("Failed to read file: " + e);
        }
    });
}

/**
 * Installs drag-and-drop file handling. Requires `dragDropEnabled: false`
 * in tauri.conf.json so the webview receives native DnD events.
 */
function installDragDrop() {
    const overlay = document.getElementById("drop-overlay");
    let dragDepth = 0;

    const showOverlay = () => {
        if (overlay) overlay.hidden = false;
    };
    const hideOverlay = () => {
        if (overlay) overlay.hidden = true;
    };

    const hasFiles = (e) => {
        const dt = e.dataTransfer;
        if (!dt) return false;
        if (dt.types && Array.from(dt.types).includes("Files")) return true;
        return false;
    };

    window.addEventListener("dragenter", (e) => {
        if (!state.currentPeer) return;
        if (!hasFiles(e)) return;
        e.preventDefault();
        dragDepth++;
        showOverlay();
    });

    window.addEventListener("dragover", (e) => {
        if (!state.currentPeer) return;
        if (!hasFiles(e)) return;
        e.preventDefault();
        if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    });

    window.addEventListener("dragleave", (e) => {
        e.preventDefault();
        dragDepth = Math.max(0, dragDepth - 1);
        if (dragDepth === 0) hideOverlay();
    });

    window.addEventListener("drop", (e) => {
        if (!state.currentPeer) return;
        if (!hasFiles(e)) return;
        e.preventDefault();
        dragDepth = 0;
        hideOverlay();
        const files = e.dataTransfer && e.dataTransfer.files;
        if (!files || files.length === 0) return;
        void dispatchFiles(files);
    });
}

function installScrollPagination() {
    const el = document.getElementById("messages");
    if (!el) return;
    el.addEventListener("scroll", () => {
        if (state.suppressScrollLoad) return;
        if (!state.currentPeer || state.currentPeer === NOTES_PEER) return;
        if (state.loadingOlder.has(state.currentPeer)) return;
        if (state.mightHaveMore[state.currentPeer] === false) return;
        if (el.scrollTop < 80) {
            loadOlder();
        }
    }, { passive: true });
}

async function init() {
    await initTheme();
    document.getElementById("theme-btn").onclick = cycleTheme;
    document.getElementById("theme-btn-login").onclick = cycleTheme;

    await installContextMenuGuard();
    installShortcutGuard();

    document.getElementById("register-btn").onclick = register;
    document.getElementById("login-btn").onclick = login;
    document.getElementById("send-btn").onclick = send;
    document.getElementById("notes-btn").onclick = openNotes;
    document.getElementById("refresh-pending-btn").onclick = refreshPending;

    wireFilePicker({
        buttonId: "attach-image-btn",
        inputId: "image-input",
        kind: "image",
        onPayload: (dataUrl) => sendImage(dataUrl),
    });
    wireFilePicker({
        buttonId: "attach-audio-btn",
        inputId: "audio-input",
        kind: "audio",
        onPayload: (dataUrl) => sendAudio(dataUrl),
    });

    document.getElementById("pending-btn").onclick = () => {
        showPendingView();
        refreshPending();
    };
    document.getElementById("pending-back-btn").onclick = () => showChatView();

    document.getElementById("settings-btn").onclick = (ev) => {
        ev.stopPropagation();
        const m = document.getElementById("settings-menu");
        const wasHidden = m.hidden;
        closeAllMenus();
        m.hidden = !wasHidden;
    };
    document.getElementById("reload-btn").onclick = () => { closeAllMenus(); reloadUI(); };
    document.getElementById("logout-btn").onclick = () => { closeAllMenus(); logout(); };
    document.getElementById("delete-btn").onclick = () => { closeAllMenus(); deleteAccount(); };

    document.getElementById("chat-menu-btn").onclick = (ev) => {
        ev.stopPropagation();
        const m = document.getElementById("chat-menu");
        const wasHidden = m.hidden;
        closeAllMenus();
        if (!state.currentPeer) return;
        m.hidden = !wasHidden;
    };
    document.getElementById("clear-chat-btn").onclick = () => { closeAllMenus(); clearChat(); };
    document.getElementById("leave-chat-btn").onclick = () => { closeAllMenus(); leaveChat(); };
    document.getElementById("block-user-btn").onclick = () => { closeAllMenus(); blockUser(); };

    document.getElementById("me-status").onclick = (ev) => {
        ev.stopPropagation();
        cycleMyStatus();
    };

    document.getElementById("new-peer-btn").onclick = () => {
        const p = document.getElementById("new-peer").value.trim();
        if (p) {
            openPeer(p);
            document.getElementById("new-peer").value = "";
        }
    };

    const msgInput = document.getElementById("msg");
    msgInput.addEventListener("keydown", (e) => {
        if (e.key === "Enter" && !e.shiftKey && !e.isComposing) {
            e.preventDefault();
            send();
        }
    });
    msgInput.addEventListener("input", () => {
        msgInput.style.height = "auto";
        msgInput.style.height = Math.min(msgInput.scrollHeight, 180) + "px";
    });

    // Paste an image directly into the composer.
    msgInput.addEventListener("paste", (e) => {
        const items = e.clipboardData && e.clipboardData.items;
        if (!items) return;
        for (const it of items) {
            if (it.kind === "file" && it.type.startsWith("image/")) {
                e.preventDefault();
                const file = it.getAsFile();
                if (!file) return;
                void dispatchFiles([file]);
                return;
            }
        }
    });

    document.getElementById("new-peer").addEventListener("keydown", (e) => {
        if (e.key === "Enter") {
            const p = e.target.value.trim();
            if (p) {
                openPeer(p);
                e.target.value = "";
            }
        }
    });

    document.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
            closeEncPanel();
            closeAllMenus();
        }
    });

    document.getElementById("peers").addEventListener("click", (e) => {
        const el = e.target.closest("[data-peer]");
        if (!el) return;
        const peer = el.dataset.peer;
        if (peer === NOTES_PEER) openNotes();
        else if (state.pending.has(peer)) acceptPending(peer);
        else openPeer(peer);
    });

    document.getElementById("messages").addEventListener("click", (e) => {
        const btn = e.target.closest("[data-act]");
        if (!btn) return;
        const msgEl = btn.closest(".msg");
        if (!msgEl) return;
        const id = msgEl.dataset.id;
        if (btn.dataset.act === "edit") editMessage(id);
        else if (btn.dataset.act === "del") deleteMessage(id);
    });

    document.getElementById("pending-list").addEventListener("click", (e) => {
        const btn = e.target.closest("[data-accept]");
        if (!btn) return;
        acceptPending(btn.dataset.accept);
    });

    document.getElementById("enc-btn").onclick = (ev) => {
        ev.stopPropagation();
        const panel = document.getElementById("enc-panel");
        if (panel.hidden) openEncPanel();
        else closeEncPanel();
    };
    document.getElementById("enc-method").onchange = (ev) => {
        document.getElementById("enc-secret-row").hidden = ev.target.value === "none";
        document.getElementById("enc-gen").hidden = ev.target.value !== "pre_shared_key";
    };
    document.getElementById("enc-cancel").onclick = closeEncPanel;
    document.getElementById("enc-apply").onclick = applyEncryption;
    document.getElementById("enc-gen").onclick = generatePsk;

    document.addEventListener("click", (ev) => {
        const widget = document.getElementById("enc-widget");
        const panel = document.getElementById("enc-panel");
        if (panel && !panel.hidden && widget && !widget.contains(ev.target)) {
            closeEncPanel();
        }
        if (!ev.target.closest(".menu-wrap")) {
            closeAllMenus();
        }
    });

    window.addEventListener("beforeunload", () => {
        try { invoke("disconnect"); } catch (_) {}
    });

    installScrollPagination();
    installDragDrop();

    await setupEvents();
    await refreshEncryptionStatus();
    renderMyStatus();
    renderMessages();
}

const STATUS_CYCLE = ["online", "away", "busy", "invisible"];

async function cycleMyStatus() {
    const idx = STATUS_CYCLE.indexOf(state.myStatus);
    const next = STATUS_CYCLE[(idx + 1) % STATUS_CYCLE.length];
    state.myStatus = next;
    renderMyStatus();
    try {
        await invoke("set_status", { status: next });
    } catch (e) {
        toast("Status error: " + e);
    }
}

init().catch((e) => {
    console.error("init failed:", e);
    toast("Initialization failed: " + e);
});