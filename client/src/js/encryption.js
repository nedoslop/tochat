// Per-chat encryption widget logic.
//
// Encryption is end-to-end and scoped to one chat at a time: Alice↔Bob
// and Alice↔Carol can each have their own key. There is no global
// encryption setting — everything here operates on `state.currentPeer`.

import { invoke } from "./api.js";
import { state, NOTES_PEER, displayName } from "./state.js";

export function encIcon(method) {
    return method === "none" ? "🔓" : "🔒";
}

/**
 * Refresh the encryption badge in the chat header for `peer` (defaults
 * to the current chat). Passing null/undefined just resets the icon.
 */
export async function refreshEncryptionStatus(peer = state.currentPeer) {
    const btn = document.getElementById("enc-btn");
    if (peer === null || peer === undefined) {
        if (btn) {
            btn.textContent = "🔓";
            btn.title = "Encryption: no chat selected";
        }
        return { method: "none", has_secret: false };
    }
    try {
        const info = await invoke("get_encryption", { peer });
        if (btn) {
            btn.textContent = encIcon(info.method);
            btn.title =
                (peer === NOTES_PEER ? "Notes" : displayName(peer)) +
                " — encryption: " + info.method;
        }
        return info;
    } catch (_) {
        return { method: "none", has_secret: false };
    }
}

export async function openEncPanel() {
    if (state.currentPeer === null) return;
    const peer = state.currentPeer;
    const info = await refreshEncryptionStatus(peer);

    const methodSel = document.getElementById("enc-method");
    methodSel.value = info.method;

    const secretRow = document.getElementById("enc-secret-row");
    secretRow.hidden = info.method === "none";

    const secretInput = document.getElementById("enc-secret");
    secretInput.value = "";
    const secretHint = document.getElementById("enc-secret-hint");
    const genBtn = document.getElementById("enc-gen");

    if (info.method === "pre_shared_key") {
        secretInput.placeholder = info.has_secret ? "(unchanged)" : "64 hex characters";
        secretHint.textContent = "Both sides must use the same 64-character hex key.";
        genBtn.hidden = false;
    } else if (info.method === "shared_password") {
        secretInput.placeholder = info.has_secret ? "(unchanged)" : "shared password";
        secretHint.textContent =
            "Both sides must use the same password. Only affects new messages.";
        genBtn.hidden = true;
    } else {
        secretInput.placeholder = "secret";
        secretHint.textContent = "Both sides must use the same secret.";
        genBtn.hidden = true;
    }

    const hint = document.getElementById("enc-hint");
    hint.textContent = "";
    hint.className = "hint";

    // Show which chat this applies to.
    const title = document.getElementById("enc-title");
    if (title) {
        const label = peer === NOTES_PEER ? "Notes" : displayName(peer);
        title.textContent = "Encryption — " + label;
    }

    document.getElementById("enc-panel").hidden = false;
}

export function closeEncPanel() {
    document.getElementById("enc-panel").hidden = true;
}

export async function applyEncryption() {
    if (state.currentPeer === null) return;
    const peer = state.currentPeer;
    const method = document.getElementById("enc-method").value;
    const raw = document.getElementById("enc-secret").value;
    const secret = raw ? raw : null;
    const hint = document.getElementById("enc-hint");

    try {
        await invoke("set_encryption", { peer, method, secret });
        await refreshEncryptionStatus(peer);
        hint.className = "hint success";
        hint.textContent = "Applied.";
        setTimeout(closeEncPanel, 600);
    } catch (e) {
        hint.className = "hint error";
        hint.textContent = String(e);
    }
}

export async function generatePsk() {
    try {
        const key = await invoke("generate_psk");
        document.getElementById("enc-secret").value = key;
    } catch (_) { /* ignore */ }
}