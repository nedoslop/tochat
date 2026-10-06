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

/**
 * Blocks browser shortcuts that don't make sense inside the app.
 * Ctrl+R / F5 / Ctrl+Shift+R — reload
 * Ctrl+Shift+I / F12 / Ctrl+Shift+C / Ctrl+Shift+J — devtools
 * Ctrl+U — view source
 * Ctrl+P / Ctrl+Shift+P — print
 * Ctrl+N / Ctrl+T / Ctrl+W / Ctrl+Shift+W — new tab/window
 * Ctrl+= / Ctrl+- / Ctrl+0 — zoom (kept unblocked, browser handled)
 */
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

async function init() {
  await initTheme();
  document.getElementById("theme-btn").onclick = cycleTheme;
  document.getElementById("theme-btn-login").onclick = cycleTheme;

  await installContextMenuGuard();
  installShortcutGuard();

  // ---- buttons ----
  document.getElementById("register-btn").onclick = register;
  document.getElementById("login-btn").onclick = login;
  document.getElementById("send-btn").onclick = send;
  document.getElementById("notes-btn").onclick = openNotes;
  document.getElementById("refresh-pending-btn").onclick = refreshPending;

  document.getElementById("pending-btn").onclick = () => {
    showPendingView();
    refreshPending();
  };
  document.getElementById("pending-back-btn").onclick = () => showChatView();

  // ---- settings menu ----
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

  // ---- chat menu ----
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

  // ---- status menu ----
  document.getElementById("me-status").onclick = (ev) => {
    ev.stopPropagation();
    cycleMyStatus();
  };

  // ---- new peer ----
  document.getElementById("new-peer-btn").onclick = () => {
    const p = document.getElementById("new-peer").value.trim();
    if (p) {
      openPeer(p);
      document.getElementById("new-peer").value = "";
    }
  };

  // ---- composer: textarea with auto-grow, Enter to send, Shift+Enter newline ----
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

  // ---- peers list (delegation) ----
  document.getElementById("peers").addEventListener("click", (e) => {
    const el = e.target.closest("[data-peer]");
    if (!el) return;
    const peer = el.dataset.peer;
    if (peer === NOTES_PEER) openNotes();
    else if (state.pending.has(peer)) acceptPending(peer);
    else openPeer(peer);
  });

  // ---- message actions ----
  document.getElementById("messages").addEventListener("click", (e) => {
    const btn = e.target.closest("[data-act]");
    if (!btn) return;
    const msgEl = btn.closest(".msg");
    if (!msgEl) return;
    const id = msgEl.dataset.id;
    if (btn.dataset.act === "edit") editMessage(id);
    else if (btn.dataset.act === "del") deleteMessage(id);
  });

  // ---- pending list ----
  document.getElementById("pending-list").addEventListener("click", (e) => {
    const btn = e.target.closest("[data-accept]");
    if (!btn) return;
    acceptPending(btn.dataset.accept);
  });

  // ---- encryption widget ----
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

  // Close menus when clicking anywhere else.
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

  // ---- async init ----
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