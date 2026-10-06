export const NOTES_PEER = "__notes__";
export const INITIAL_LIMIT = 50;

export const state = {
    me: null,
    currentPeer: null,
    peers: new Set(),
    pending: new Set(),
    blocked: new Set(),
    online: new Set(),
    peerStatus: {},
    profiles: {},
    msgCache: {},
    pulling: new Set(),   // kept for compatibility; not used as a guard
    unread: {},
    myStatus: "online",
    mightHaveMore: {},
    loadingOlder: new Set(),
    lastPullLimit: {},
    suppressScrollLoad: false,
    seenIds: {},
    pendingOutgoingProfile: null,
};

export function resetState() {
    state.me = null;
    state.currentPeer = null;
    state.peers.clear();
    state.pending.clear();
    state.blocked.clear();
    state.online.clear();
    state.peerStatus = {};
    state.profiles = {};
    state.pulling.clear();
    state.unread = {};
    state.mightHaveMore = {};
    state.loadingOlder.clear();
    state.lastPullLimit = {};
    state.suppressScrollLoad = false;
    state.pendingOutgoingProfile = null;
    for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
    for (const k of Object.keys(state.seenIds)) delete state.seenIds[k];
}

export function displayName(username) {
    if (!username) return "";
    if (username === NOTES_PEER) return "Notes";
    const p = state.profiles[username];
    if (p && p.display_name && p.display_name.trim()) return p.display_name;
    return username;
}

export function avatarFor(username) {
    if (!username) return null;
    if (username === NOTES_PEER) return null;
    const p = state.profiles[username];
    return p && p.avatar ? p.avatar : null;
}

export function totalUnread() {
    let n = 0;
    for (const k of Object.keys(state.unread)) n += state.unread[k] || 0;
    return n;
}
