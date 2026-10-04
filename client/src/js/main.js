// Entry point — wires DOM events, then sets up Tauri listeners.

import { invoke } from "./api.js";
import { state } from "./state.js";
import { toast } from "./utils.js";
import {
  register,
  login,
  send,
  logout,
  reloadUI,
  deleteAccount,
  openPeer,
  editMessage,
  deleteMessage,
  acceptPending,
  refreshPending,
} from "./actions.js";
import {
  renderMessages,
  showChatView,
  showPendingView,
} from "./ui.js";
import {
  refreshEncryptionStatus,
  openEncPanel,
  closeEncPanel,
  applyEncryption,
} from "./encryption.js";
import { setupEvents } from "./events.js";

async function init() {
  // ---- buttons ----
  document.getElementById("register-btn").onclick = register;
  document.getElementById("login-btn").onclick = login;
  document.getElementById("send-btn").onclick = send;
  document.getElementById("logout-btn").onclick = logout;
  document.getElementById("reload-btn").onclick = reloadUI;
  document.getElementById("delete-btn").onclick = deleteAccount;
  document.getElementById("refresh-pending-btn").onclick = refreshPending;

  document.getElementById("pending-btn").onclick = () => {
    showPendingView();
    refreshPending();
  };
  document.getElementById("pending-back-btn").onclick = () => showChatView();

  document.getElementById("new-peer-btn").onclick = () => {
    const p = document.getElementById("new-peer").value.trim();
    if (p) {
      openPeer(p);
      document.getElementById("new-peer").value = "";
    }
  };

  // ---- keyboard ----
  document.getElementById("msg").addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      send();
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
    if (e.key === "Escape") closeEncPanel();
  });

  // ---- peer list (event delegation) ----
  document.getElementById("peers").addEventListener("click", (e) => {
    const el = e.target.closest("[data-peer]");
    if (!el) return;
    const peer = el.dataset.peer;
    if (state.pending.has(peer)) acceptPending(peer);
    else openPeer(peer);
  });

  // ---- message actions (event delegation) ----
  document.getElementById("messages").addEventListener("click", (e) => {
    const btn = e.target.closest("[data-act]");
    if (!btn) return;
    const msgEl = btn.closest(".msg");
    if (!msgEl) return;
    const id = msgEl.dataset.id;
    if (btn.dataset.act === "edit") editMessage(id);
    else if (btn.dataset.act === "del") deleteMessage(id);
  });

  // ---- pending list (event delegation) ----
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
    document.getElementById("enc-secret-row").hidden =
      ev.target.value === "none";
  };

  document.getElementById("enc-cancel").onclick = closeEncPanel;
  document.getElementById("enc-apply").onclick = applyEncryption;

  document.addEventListener("click", (ev) => {
    const widget = document.getElementById("enc-widget");
    const panel = document.getElementById("enc-panel");
    if (!widget || !panel) return;
    if (panel.hidden) return;
    if (widget.contains(ev.target)) return;
    closeEncPanel();
  });

  window.addEventListener("beforeunload", () => {
    try { invoke("disconnect"); } catch (_) {}
  });

  // ---- async init ----
  await setupEvents();
  await refreshEncryptionStatus();
  renderMessages();
}

init().catch((e) => {
  console.error("init failed:", e);
  toast("Initialization failed: " + e);
});