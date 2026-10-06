// Telegram-style incoming-message toasts.
//
// These are visually distinct from the bottom-right `#toast` used for
// system messages. They appear top-right, stack vertically, show the
// sender's avatar, and auto-dismiss. Clicking one opens the chat.

import { state, NOTES_PEER } from "./state.js";
import { avatarColor, initial } from "./utils.js";

const CONTAINER_ID = "notif-stack";
const DISMISS_MS = 5000;
const MAX_VISIBLE = 4;

/** peer -> { el, timer, generation } of the live toast for that peer. */
const live = new Map();

function ensureContainer() {
    let el = document.getElementById(CONTAINER_ID);
    if (el) return el;
    el = document.createElement("div");
    el.id = CONTAINER_ID;
    el.className = "notif-stack";
    document.body.appendChild(el);
    return el;
}

/**
 * Shows (or updates) an incoming-message toast.
 *
 * @param {object} opts
 * @param {string} opts.peer       sender username (or NOTES_PEER)
 * @param {string} opts.text       plaintext preview
 * @param {string} [opts.kind]     "text" | "image" | "audio" | "file"
 * @param {() => void} [opts.onClick]  called when clicked
 */
export function showMessageToast({ peer, text, kind, onClick }) {
    // If the same peer already has a toast, replace its content and reset
    // the timer, moving it to the bottom of the stack.
    const existing = live.get(peer);
    if (existing) {
        clearTimeout(existing.timer);
        existing.el.remove();
        live.delete(peer);
    }

    const stack = ensureContainer();

    const el = document.createElement("div");
    el.className = "notif";

    const av = document.createElement("div");
    av.className = "notif-avatar";
    if (peer === NOTES_PEER) {
        av.textContent = "📝";
        av.style.background = "#8b5cf6";
    } else {
        av.textContent = initial(peer);
        av.style.background = avatarColor(peer);
    }

    const body = document.createElement("div");
    body.className = "notif-body";

    const name = document.createElement("div");
    name.className = "notif-name";
    name.textContent = peer === NOTES_PEER ? "Notes" : peer;

    const txt = document.createElement("div");
    txt.className = "notif-text";
    txt.textContent = previewFor(kind, text);

    body.appendChild(name);
    body.appendChild(txt);

    const close = document.createElement("button");
    close.className = "notif-close";
    close.type = "button";
    close.title = "Dismiss";
    close.textContent = "×";

    el.appendChild(av);
    el.appendChild(body);
    el.appendChild(close);

    const dismiss = () => {
        const entry = live.get(peer);
        if (!entry || entry.el !== el) return;
        live.delete(peer);
        clearTimeout(entry.timer);
        el.classList.add("leaving");
        setTimeout(() => el.remove(), 200);
    };

    close.addEventListener("click", (e) => {
        e.stopPropagation();
        dismiss();
    });

    el.addEventListener("click", () => {
        dismiss();
        if (onClick) onClick();
    });

    stack.appendChild(el);

    // Cap the number of visible toasts; drop the oldest if we exceed the cap.
    while (stack.children.length > MAX_VISIBLE) {
        const first = stack.firstElementChild;
        if (first) first.remove();
    }

    const timer = setTimeout(dismiss, DISMISS_MS);
    live.set(peer, { el, timer });
}

/** Clears all visible toasts (used on logout / disconnect). */
export function clearAllToasts() {
    for (const { el, timer } of live.values()) {
        clearTimeout(timer);
        el.remove();
    }
    live.clear();
}

function previewFor(kind, text) {
    if (kind === "image") return "📷 Photo";
    if (kind === "audio") return "🎤 Audio";
    if (kind === "file") return "📎 File";
    if (!text) return "(empty)";
    if (text.length > 120) return text.slice(0, 117) + "...";
    return text;
}