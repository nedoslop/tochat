import { invoke } from "./api.js";
import { state, NOTES_PEER } from "./state.js";
import { toast } from "./utils.js";
import {
    register, login, send, logout, reloadUI, deleteAccount,
    openPeer, openNotes, openPeerByName, editMessage, deleteMessage,
    acceptPending, refreshPending, clearChat, leaveChat, blockUser,
    loadOlder, sendImage, sendAudio, sendFile, updateBadge,
    openSidebar, closeSidebar,
} from "./actions.js";
import {
    renderMessages, renderMyStatus, showChatView, showPendingView,
    closeImageViewer,
} from "./ui.js";
import {
    refreshEncryptionStatus, openEncPanel, closeEncPanel,
    applyEncryption, generatePsk,
} from "./encryption.js";
import { initTheme, cycleTheme } from "./theme.js";
import { setupEvents } from "./events.js";

// ---------- profile modal ----------

// pendingAvatar semantics:
//   undefined → user didn't touch the avatar; keep whatever we have.
//   null      → user removed the avatar; send null.
//   string    → new avatar data URL to send.
let pendingAvatar = undefined;

function openProfileModal() {
    const modal = document.getElementById("profile-modal");
    const nameInput = document.getElementById("profile-name");
    const imgEl = document.getElementById("profile-avatar-img");
    const phEl = document.getElementById("profile-avatar-placeholder");
    const hint = document.getElementById("profile-hint");

    const mine = state.profiles[state.meId] || {};
    nameInput.value = mine.display_name || "";
    pendingAvatar = undefined;
    hint.hidden = true;

    if (mine.avatar) {
        imgEl.src = mine.avatar;
        imgEl.hidden = false;
        phEl.hidden = true;
    } else {
        imgEl.hidden = true;
        phEl.hidden = false;
        phEl.textContent = (state.meName || "?").charAt(0).toUpperCase();
    }
    modal.hidden = false;
    requestAnimationFrame(() => nameInput.focus());
}

function closeProfileModal() {
    document.getElementById("profile-modal").hidden = true;
}

/** Resize image to 128×128 max, return data URL (JPEG). */
async function resizeAvatar(file) {
    const dataUrl = await new Promise((resolve, reject) => {
        const r = new FileReader();
        r.onload = () => resolve(String(r.result || ""));
        r.onerror = () => reject(r.error || new Error("read error"));
        r.readAsDataURL(file);
    });
    const img = await new Promise((resolve, reject) => {
        const i = new Image();
        i.onload = () => resolve(i);
        i.onerror = () => reject(new Error("invalid image"));
        i.src = dataUrl;
    });
    const size = 128;
    const canvas = document.createElement("canvas");
    canvas.width = size;
    canvas.height = size;
    const ctx = canvas.getContext("2d");
    const scale = Math.max(size / img.width, size / img.height);
    const dw = img.width * scale, dh = img.height * scale;
    const dx = (size - dw) / 2, dy = (size - dh) / 2;
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, size, size);
    ctx.drawImage(img, dx, dy, dw, dh);
    return canvas.toDataURL("image/jpeg", 0.82);
}

async function saveProfile() {
    const hint = document.getElementById("profile-hint");
    const name = document.getElementById("profile-name").value.trim();

    // Always send BOTH fields with their current value. The server does a
    // full replace, so a missing field would wipe the column.
    const currentAvatar = state.profiles[state.meId]?.avatar ?? null;
    const avatarToSend = pendingAvatar === undefined ? currentAvatar : pendingAvatar;

    // NOTE: Tauri 2 converts Rust snake_case param names to camelCase for
    // JS. `display_name` in Rust → `displayName` in JS. Sending
    // `display_name` would be silently ignored (or default to None).
    const payload = {
        displayName: name || null,
        avatar: avatarToSend,
    };

    try {
        await invoke("set_profile", payload);
        // Optimistic local update; the server also echoes the authoritative
        // value back through the "profile" event.
        state.profiles[state.meId] = {
            username: state.meName,
            display_name: name || null,
            avatar: avatarToSend,
        };
        closeProfileModal();
        toast("Profile updated.");
    } catch (e) {
        hint.textContent = String(e);
        hint.hidden = false;
    }
}

// ---------- boot ----------

async function installContextMenuGuard() {
    try {
        const isRelease = await invoke("is_release");
        if (isRelease) document.addEventListener("contextmenu", (e) => e.preventDefault());
    } catch (_) {}
}

function installShortcutGuard() {
    const block = (e) => {
        const k = e.key;
        const ctrl = e.ctrlKey || e.metaKey;
        if (
            k === "F5" || k === "F12" ||
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
        ) { e.preventDefault(); e.stopPropagation(); }
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
            if (kind === "image") {
                const dataUrl = await readAsDataUrl(file);
                await sendImage(dataUrl);
            } else if (kind === "audio") {
                const dataUrl = await readAsDataUrl(file);
                await sendAudio(dataUrl);
            } else {
                await sendFile(file);
            }
        } catch (e) {
            toast("Send file error: " + e);
        }
    }
}

function wireFilePicker({ buttonId, inputId, kind, onPayload }) {
    const btn = document.getElementById(buttonId);
    const input = document.getElementById(inputId);
    if (!btn || !input) return;

    btn.addEventListener("click", () => { input.value = ""; input.click(); });

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
        } catch (e) { toast("Failed to read file: " + e); }
    });
}

function installDragDrop() {
    const overlay = document.getElementById("drop-overlay");
    let dragDepth = 0;

    const showOverlay = () => { if (overlay) overlay.hidden = false; };
    const hideOverlay = () => { if (overlay) overlay.hidden = true; };

    const hasFiles = (e) => {
        const dt = e.dataTransfer;
        if (!dt) return false;
        if (dt.types && Array.from(dt.types).includes("Files")) return true;
        return false;
    };

    window.addEventListener("dragenter", (e) => {
        if (state.currentPeer === null) return;
        if (!hasFiles(e)) return;
        e.preventDefault();
        dragDepth++; showOverlay();
    });
    window.addEventListener("dragover", (e) => {
        if (state.currentPeer === null) return;
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
        if (state.currentPeer === null) return;
        if (!hasFiles(e)) return;
        e.preventDefault();
        dragDepth = 0; hideOverlay();
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
        if (state.currentPeer === null || state.currentPeer === NOTES_PEER) return;
        if (state.loadingOlder.has(state.currentPeer)) return;
        if (state.mightHaveMore[state.currentPeer] === false) return;
        if (el.scrollTop < 80) loadOlder();
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
        onPayload: (d) => sendImage(d),
    });
    wireFilePicker({
        buttonId: "attach-audio-btn",
        inputId: "audio-input",
        kind: "audio",
        onPayload: (d) => sendAudio(d),
    });

    // Any-file picker.
    {
        const btn = document.getElementById("attach-file-btn");
        const input = document.getElementById("any-file-input");
        btn.addEventListener("click", () => { input.value = ""; input.click(); });
        input.addEventListener("change", async () => {
            const files = input.files;
            if (!files || files.length === 0) return;
            const file = files[0];
            if (file.size > MAX_FILE_BYTES) {
                toast(`File too large (max ${Math.round(MAX_FILE_BYTES / (1024 * 1024))} MiB).`);
                return;
            }
            try { await sendFile(file); } catch (e) { toast("Send file error: " + e); }
        });
    }

    document.getElementById("pending-btn").onclick = () => { showPendingView(); refreshPending(); };
    document.getElementById("pending-back-btn").onclick = () => showChatView();

    // Hamburger / sidebar drawer (mobile).
    document.getElementById("hamburger-btn").onclick = openSidebar;
    document.getElementById("sidebar-backdrop").onclick = closeSidebar;

    document.getElementById("settings-btn").onclick = (ev) => {
        ev.stopPropagation();
        const m = document.getElementById("settings-menu");
        const wasHidden = m.hidden;
        closeAllMenus();
        m.hidden = !wasHidden;
    };
    document.getElementById("edit-profile-btn").onclick = () => { closeAllMenus(); openProfileModal(); };
    document.getElementById("reload-btn").onclick = () => { closeAllMenus(); reloadUI(); };
    document.getElementById("logout-btn").onclick = () => { closeAllMenus(); logout(); };

    document.getElementById("chat-menu-btn").onclick = (ev) => {
        ev.stopPropagation();
        const m = document.getElementById("chat-menu");
        const wasHidden = m.hidden;
        closeAllMenus();
        if (state.currentPeer === null) return;
        m.hidden = !wasHidden;
    };
    document.getElementById("clear-chat-btn").onclick = () => { closeAllMenus(); clearChat(); };
    document.getElementById("leave-chat-btn").onclick = () => { closeAllMenus(); leaveChat(); };
    document.getElementById("block-user-btn").onclick = () => { closeAllMenus(); blockUser(); };

    // Profile modal wiring.
    const profileModal = document.getElementById("profile-modal");
    const profileAvatarInput = document.getElementById("profile-avatar-input");
    document.getElementById("profile-cancel").onclick = closeProfileModal;
    document.getElementById("profile-save").onclick = saveProfile;
    document.getElementById("profile-avatar-choose").onclick = () => profileAvatarInput.click();
    document.getElementById("profile-avatar-remove").onclick = () => {
        pendingAvatar = null;
        document.getElementById("profile-avatar-img").hidden = true;
        const ph = document.getElementById("profile-avatar-placeholder");
        ph.hidden = false;
        ph.textContent = (state.meName || "?").charAt(0).toUpperCase();
    };
    document.getElementById("profile-delete").onclick = () => {
        closeProfileModal();
        deleteAccount();
    };
    profileAvatarInput.addEventListener("change", async () => {
        const f = profileAvatarInput.files && profileAvatarInput.files[0];
        if (!f) return;
        try {
            const url = await resizeAvatar(f);
            pendingAvatar = url;
            const imgEl = document.getElementById("profile-avatar-img");
            imgEl.src = url;
            imgEl.hidden = false;
            document.getElementById("profile-avatar-placeholder").hidden = true;
        } catch (e) { toast("Image error: " + e); }
    });
    profileModal.addEventListener("click", (ev) => {
        if (ev.target === profileModal) closeProfileModal();
    });

    // Image viewer.
    document.getElementById("image-viewer").addEventListener("click", (ev) => {
        if (ev.target.id === "image-viewer" || ev.target.id === "image-viewer-close") {
            closeImageViewer();
        }
    });

    document.getElementById("me-status").onclick = (ev) => { ev.stopPropagation(); cycleMyStatus(); };

    document.getElementById("new-peer-btn").onclick = () => {
        const p = document.getElementById("new-peer").value.trim();
        if (p) {
            void openPeerByName(p);
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
        msgInput.style.height = Math.min(msgInput.scrollHeight + 2, 180) + "px";
    });

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
                void openPeerByName(p);
                e.target.value = "";
            }
        }
    });

    document.addEventListener("keydown", (e) => {
        if (e.key === "Escape") {
            closeEncPanel();
            closeAllMenus();
            closeProfileModal();
            closeImageViewer();
            closeSidebar();
        }
    });

    document.getElementById("peers").addEventListener("click", (e) => {
        const el = e.target.closest("[data-peer]");
        if (!el) return;
        const peerId = Number(el.dataset.peer);
        if (peerId === NOTES_PEER) openNotes();
        else if (state.pending.has(peerId)) acceptPending(peerId);
        else openPeer(peerId);
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
        acceptPending(Number(btn.dataset.accept));
    });

    document.getElementById("enc-btn").onclick = (ev) => {
        ev.stopPropagation();
        const panel = document.getElementById("enc-panel");
        if (panel.hidden) openEncPanel(); else closeEncPanel();
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
        if (panel && !panel.hidden && widget && !widget.contains(ev.target)) closeEncPanel();
        if (!ev.target.closest(".menu-wrap")) closeAllMenus();
    });

    window.addEventListener("beforeunload", () => { try { invoke("disconnect"); } catch (_) {} });

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
    try { await invoke("set_status", { status: next }); }
    catch (e) { toast("Status error: " + e); }
}

init().catch((e) => {
    console.error("init failed:", e);
    toast("Initialization failed: " + e);
});