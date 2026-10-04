// Single shared mutable state object. Everything else imports this and
// mutates it in place.

export const state = {
  /** Currently logged-in username (or null). */
  me: null,
  /** Username of the currently-open chat (or null). */
  currentPeer: null,
  /** Established peers (usernames). */
  peers: new Set(),
  /** Users who messaged us first — pending acceptance. */
  pending: new Set(),
  /** Peers currently believed to be online. */
  online: new Set(),
  /** peer -> array of LocalMsg-shaped objects. */
  msgCache: {},
  /** Peers for which a pull request is in flight. */
  pulling: new Set(),
};

export function resetState() {
  state.me = null;
  state.currentPeer = null;
  state.peers.clear();
  state.pending.clear();
  state.online.clear();
  state.pulling.clear();
  for (const k of Object.keys(state.msgCache)) delete state.msgCache[k];
}