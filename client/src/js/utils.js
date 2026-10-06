export function escapeHtml(text) {
    const div = document.createElement("div");
    div.textContent = text;
    return div.innerHTML;
}

let toastTimer = null;
export function toast(text, ms = 3500) {
    const t = document.getElementById("toast");
    if (!t) return;
    t.textContent = text;
    t.hidden = false;
    clearTimeout(toastTimer);
    toastTimer = setTimeout(() => { t.hidden = true; }, ms);
}

export function nowMs() { return Date.now(); }

const AVATAR_COLORS = [
    "#6366f1", "#8b5cf6", "#ec4899", "#f43f5e", "#f59e0b",
    "#10b981", "#06b6d4", "#3b82f6", "#84cc16", "#ef4444",
];

export function avatarColor(name) {
    const s = name || "";
    let h = 0;
    for (let i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0;
    return AVATAR_COLORS[Math.abs(h) % AVATAR_COLORS.length];
}

export function initial(name) {
    const t = (name || "").trim();
    return t ? t.charAt(0).toUpperCase() : "?";
}

export function formatTime(ts) {
    return new Date(ts).toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
}

export function formatDate(ts) {
    return new Date(ts).toLocaleDateString();
}

/** Human-readable byte size. */
export function formatBytes(n) {
    if (!n || n < 0) return "";
    const units = ["B", "KB", "MB", "GB"];
    let i = 0;
    while (n >= 1024 && i < units.length - 1) { n /= 1024; i++; }
    return `${n.toFixed(n < 10 && i > 0 ? 1 : 0)} ${units[i]}`;
}

/**
 * Parses a `file`-kind payload. New messages use JSON:
 *   {"name": "...", "size": 1234, "data": "data:..."}
 * Old messages (or unexpected shapes) fall back to a plain data URL.
 */
export function parseFilePayload(payload) {
    try {
        const v = JSON.parse(payload);
        if (v && typeof v === "object" && typeof v.data === "string") {
            return {
                name: typeof v.name === "string" && v.name ? v.name : "file",
                size: typeof v.size === "number" ? v.size : null,
                data: v.data,
            };
        }
    } catch (_) {}
    return { name: "file", size: null, data: payload };
}

/** Approximate decoded size (bytes) of a base64 data URL. */
export function dataUrlBytes(dataUrl) {
    const idx = dataUrl.indexOf(",");
    if (idx < 0) return 0;
    const b64 = dataUrl.slice(idx + 1);
    const len = b64.length;
    const pad = b64.endsWith("==") ? 2 : b64.endsWith("=") ? 1 : 0;
    return Math.max(0, Math.floor(len * 3 / 4) - pad);
}
