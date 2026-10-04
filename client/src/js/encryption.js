// Encryption widget logic — pure UI + one Tauri call.

import { invoke } from "./api.js";

export function encIcon(method) {
  return method === "none" ? "🔓" : "🔒";
}

export async function refreshEncryptionStatus() {
  try {
    const info = await invoke("get_encryption");
    const btn = document.getElementById("enc-btn");
    if (btn) {
      btn.textContent = encIcon(info.method);
      btn.title = "Encryption: " + info.method;
    }
    return info;
  } catch (_) {
    return { method: "none", has_secret: false };
  }
}

export async function openEncPanel() {
  const info = await refreshEncryptionStatus();

  const methodSel = document.getElementById("enc-method");
  methodSel.value = info.method;

  const secretRow = document.getElementById("enc-secret-row");
  secretRow.hidden = info.method === "none";

  const secretInput = document.getElementById("enc-secret");
  secretInput.value = "";
  const secretHint = document.getElementById("enc-secret-hint");

  if (info.method === "pre_shared_key") {
    secretInput.placeholder = info.has_secret ? "(unchanged)" : "64 hex characters";
    secretHint.textContent = "Both sides must use the same 64-character hex key.";
  } else if (info.method === "shared_password") {
    secretInput.placeholder = info.has_secret ? "(unchanged)" : "shared password";
    secretHint.textContent =
      "Both sides must use the same password. Only affects new messages.";
  } else {
    secretInput.placeholder = "secret";
    secretHint.textContent = "Both sides must use the same secret.";
  }

  const hint = document.getElementById("enc-hint");
  hint.textContent = "";
  hint.className = "hint";

  document.getElementById("enc-panel").hidden = false;
}

export function closeEncPanel() {
  document.getElementById("enc-panel").hidden = true;
}

export async function applyEncryption() {
  const method = document.getElementById("enc-method").value;
  const raw = document.getElementById("enc-secret").value;
  const secret = raw ? raw : null;
  const hint = document.getElementById("enc-hint");

  try {
    await invoke("set_encryption", { method, secret });
    await refreshEncryptionStatus();
    hint.className = "hint success";
    hint.textContent = "Applied.";
    setTimeout(closeEncPanel, 600);
  } catch (e) {
    hint.className = "hint error";
    hint.textContent = String(e);
  }
}