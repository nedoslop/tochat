// Single shared mutable state object.

export const NOTES_PEER = "__notes__";

/** How many messages to fetch per page for scroll-back pagination. */
export const INITIAL_LIMIT = 50;

export const state = {
    me: null,
    currentPeer: null,
    peers: new Set(),
    pending: new Set(),
    blocked: new Set(),
    online: new Set(),
    peerStatus: {},
    msgCache: {},
    pulling: new Set(),
    unread: {},
    myStatus: "online",
    mightHaveMore: {},
    loadingOlder: new Set(),
    lastPullLimit: {},
    suppressScrollLoad: false,
    /**
     * peer -> Set of message ids already rendered at least once. Used to
     * skip the entry animation for messages that were already on screen —
     * this is what prevents the visual "blink" during re-renders.
     */
    seenIds: {},
};

export function resetState() {
    state.me = null;
    state.currentPeer = null;
    state.peers.clear();
    state.pending.clear();
    state.blocked.clear();
    state.online.clear();
    state.peerStatus = {};
    state.pulling.clear();
    state.unread = {};
    state.mightHaveMore = {};
    state.loadingOlder.clear();
    state.lastPullLimit = {};
    state.suppressScrollLoad = false;
    for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
    for (const k of Object.keys(state.seenIds)) delete state.seenIds[k];
}