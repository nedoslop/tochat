export const NOTES_PEER = 0;
export const INITIAL_LIMIT = 50;

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

    myStatus: "online",

    // id -> bool
    mightHaveMore: {},

    loadingOlder: new Set(),
    lastPullLimit: {},
    suppressScrollLoad: false,

    // id -> Set<msg-id>
    seenIds: {},
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
    for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
    for (const k of Object.keys(state.seenIds)) delete state.seenIds[k];
}

export function displayName(peerId) {
    if (peerId === null || peerId === undefined) return "";
    if (peerId === NOTES_PEER) return "Notes";
    const p = state.profiles[peerId];
    if (p) {
        if (p.display_name && p.display_name.trim()) return p.display_name;
        if (p.username) return p.username;
    }
    return `#${peerId}`;
}

export function avatarFor(peerId) {
    if (!peerId) return null;
    if (peerId === NOTES_PEER) return null;
    const p = state.profiles[peerId];
    return p && p.avatar ? p.avatar : null;
}

export function totalUnread() {
    let n = 0;
    for (const k of Object.keys(state.unread)) n += state.unread[k] || 0;
    return n;
}