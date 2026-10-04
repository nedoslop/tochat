// User-initiated actions: register, login, send, edit, delete, pull, logout.

import { invoke } from "./api.js";
import { state, resetState } from "./state.js";
import { toast } from "./utils.js";
import {
  renderSidebar,
  renderMessages,
  renderPendingList,
  showChatView,
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
  const pw = prompt("Enter your password to delete the account");
  if (pw === null) return;
  try {
    await invoke("delete_account", { password: pw });
  } catch (e) {
    toast("Delete error: " + e);
  }
}

// ---------- chats ----------

export async function openPeer(peer) {
  if (!peer) return;
  if (peer === state.me) return toast("You can't chat with yourself.");

  state.pending.delete(peer);
  state.currentPeer = peer;
  state.peers.add(peer);
  showChatView();

  try {
    state.msgCache[peer] = await invoke("get_messages", { peer });
  } catch (_) {
    state.msgCache[peer] = state.msgCache[peer] || [];
  }

  renderSidebar();
  renderMessages();
  autoPull(peer);
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
    state.pending.delete(state.currentPeer);
    state.msgCache[state.currentPeer] = await invoke("get_messages", {
      peer: state.currentPeer,
    });
    renderSidebar();
    renderMessages();
  } catch (e) {
    toast("Send error: " + e);
  }
}

export async function editMessage(id) {
  const peer = state.currentPeer;
  if (!peer) return;

  const msg = (state.msgCache[peer] || []).find((m) => m.id === id);
  const currentText = msg ? msg.payload : "";

  const next = prompt("Edit message (leave empty to delete):", currentText);
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
  if (!confirm("Delete this message?")) return;
  try {
    await invoke("delete_message", { peer, id });
    state.msgCache[peer] = await invoke("get_messages", { peer });
    renderMessages();
  } catch (e) {
    toast("Delete error: " + e);
  }
}

// ---------- history sync ----------

/**
 * Asks `peer` for any messages we don't already have.
 *
 * `since` is the largest `edit_ts` we've seen *for inbound messages from
 * that peer*. Using inbound-only means:
 *   - We don't leak our local outbound edit_ts into the cutoff.
 *   - If we sent something offline and it later gets pulled back from the
 *     peer (who received it), it will have a fresh edit_ts and be included.
 */
export async function autoPull(peer) {
  if (!peer || peer === state.me) return;
  if (state.pulling.has(peer)) return;

  state.pulling.add(peer);
  try {
    if (!state.msgCache[peer]) {
      state.msgCache[peer] = await invoke("get_messages", { peer });
    }
    const since = lastReceivedEditTs(peer);
    await invoke("pull_history", { from: peer, since });
  } catch (_) {
    state.pulling.delete(peer);
    return;
  }
  // Safety net in case the response never arrives.
  setTimeout(() => state.pulling.delete(peer), 6000);
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
  renderSidebar();
  renderMessages();
}