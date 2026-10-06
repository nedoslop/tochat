// Single shared mutable state object.

export const NOTES_PEER = "__notes__";

/** How many messages to fetch at a time for scroll-back pagination. */
export const INITIAL_LIMIT = 50;

export const state = {
  me: null,
  currentPeer: null,
  peers: new Set(),
  pending: new Set(),
  blocked: new Set(),
  online: new Set(),
  /** peer -> status string ("online" | "away" | "busy" | "invisible"). */
  peerStatus: {},
  /** peer -> array of LocalMsg-shaped objects. */
  msgCache: {},
  /** Peers for which a pull request is in flight. */
  pulling: new Set(),
  /** peer -> unread message count. */
  unread: {},
  /** My own status. */
  myStatus: "online",
  /**
   * peer -> boolean. `false` means we know we've reached the start of
   * history; `undefined` or `true` means there may be more. Populated by
   * bounded pulls (initial load, loadOlder).
   */
  mightHaveMore: {},
  /** Peers with a `loadOlder` request currently in flight. */
  loadingOlder: new Set(),
  /** peer -> the limit used for the last bounded pull (null if unbounded). */
  lastPullLimit: {},
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
  for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
}