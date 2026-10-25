export const NOTES_PEER = 0;
export const INITIAL_LIMIT = 50;

const STATUS_KEY = "chat_my_status";
const VALID_STATUS = new Set(["online", "away", "busy", "invisible"]);

function loadMyStatus() {
    try {
        const s = localStorage.getItem(STATUS_KEY);
        if (s && VALID_STATUS.has(s)) return s;
    } catch (_) { /* localStorage unavailable */ }
    return "online";
}

export function persistMyStatus(status) {
    try { localStorage.setItem(STATUS_KEY, status); } catch (_) {}
}

export const state = {
    // Logged-in user (id + username). Populated on auth-ok.
    meId: null,
    meName: null,

    // Current chat peer ID (number) or NOTES_PEER (0).
    currentPeer: null,

    // Sets of numeric IDs.
    peers: new Set(),
    pending: new Set(),
    blocked: new Set(),
    online: new Set(),

    // id -> "online" | "away" | "busy" | "invisible"
    peerStatus: {},

    // id -> { username, display_name, avatar }
    profiles: {},

    // username -> id (for reverse lookups of freshly typed names)
    nameToId: new Map(),

    // id -> [LocalMsg]
    msgCache: {},

    // id -> unread count
    unread: {},

    // Persisted across restarts so `busy` survives reconnects.
    myStatus: loadMyStatus(),

    // id -> bool
    mightHaveMore: {},

    loadingOlder: new Set(),
    lastPullLimit: {},
    suppressScrollLoad: false,

    // id -> Set<msg-id>
    seenIds: {},

    // Set of peer IDs currently syncing messages
    syncingPeers: new Set(),

    // OS notification permission state
    notificationsEnabled: true,
};

export function resetState() {
    state.meId = null;
    state.meName = null;
    state.currentPeer = null;
    state.peers.clear();
    state.pending.clear();
    state.blocked.clear();
    state.online.clear();
    state.peerStatus = {};
    state.profiles = {};
    state.nameToId.clear();
    state.unread = {};
    state.mightHaveMore = {};
    state.loadingOlder.clear();
    state.lastPullLimit = {};
    state.suppressScrollLoad = false;
    state.syncingPeers.clear();
    state.notificationsEnabled = true;
    for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
    for (const k of Object.keys(state.seenIds)) delete state.seenIds[k];
}

export function displayName(peerId) {
    const num = Number(peerId);
    if (!Number.isInteger(num) || num < 0) return "";
    if (num === NOTES_PEER) return "Notes";
    const p = state.profiles[num];
    if (p) {
        if (p.display_name && p.display_name.trim()) return p.display_name.trim();
        if (p.username && p.username.trim()) return p.username.trim();
    }
    return `#${num}`;
}

export function avatarFor(peerId) {
    const num = Number(peerId);
    if (!Number.isInteger(num) || num <= 0) return null;
    const p = state.profiles[num];
    return p && p.avatar ? p.avatar : null;
}

export function totalUnread() {
    let n = 0;
    for (const k of Object.keys(state.unread)) n += state.unread[k] || 0;
    return n;
}