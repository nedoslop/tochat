// Single shared mutable state object.

export const NOTES_PEER = "__notes__";

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
  for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
}