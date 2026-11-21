use crate::db::Database;
use crate::protocol::{ServerMsg, UserStatus};
use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::mpsc;

/// One active WebSocket session belonging to a logged-in user.
#[derive(Clone)]
pub struct OnlineSession {
    pub tx: mpsc::UnboundedSender<ServerMsg>,
    pub session_id: u64,
    pub status: UserStatus,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    /// Per-user list of active sessions (multi-device).
    pub online: Arc<DashMap<i64, Vec<OnlineSession>>>,
    /// Last known status per user; survives reconnects, cleared on restart.
    pub user_status: Arc<DashMap<i64, UserStatus>>,
    /// Outstanding history pulls, keyed by
    /// `(requester, responder, responder_session_id)` with the number of
    /// forwarded requests not yet answered.
    ///
    /// Per-SESSION accounting matters: a pull is forwarded to every session
    /// of the responder (different devices may hold different parts of the
    /// history), and each session's reply must consume its OWN pull. With a
    /// single per-user counter, the second device's reply was dropped as
    /// "unsolicited" and its messages were silently lost.
    pub pulls: Arc<DashMap<(i64, i64, u64), u32>>,
}

impl AppState {
    pub fn is_online(&self, user_id: i64) -> bool {
        self.online
            .get(&user_id)
            .map(|v| v.iter().any(|s| !s.tx.is_closed()))
            .unwrap_or(false)
    }

    pub fn status_of(&self, user_id: i64) -> UserStatus {
        self.user_status
            .get(&user_id)
            .map(|v| *v)
            .unwrap_or(UserStatus::Online)
    }

    /// Send `msg` to every session of `user_id`. Returns # of sessions reached.
    pub fn send_to_user(&self, user_id: i64, msg: ServerMsg) -> usize {
        let Some(mut sessions) = self.online.get_mut(&user_id) else {
            return 0;
        };
        sessions.retain(|s| !s.tx.is_closed());
        let mut n = 0;
        for s in sessions.iter() {
            if s.tx.send(msg.clone()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Send `msg` to every session of `user_id` EXCEPT `except_session`.
    pub fn send_to_user_except(&self, user_id: i64, except_session: u64, msg: ServerMsg) -> usize {
        let Some(mut sessions) = self.online.get_mut(&user_id) else {
            return 0;
        };
        sessions.retain(|s| !s.tx.is_closed());
        let mut n = 0;
        for s in sessions.iter() {
            if s.session_id == except_session {
                continue;
            }
            if s.tx.send(msg.clone()).is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Like `send_to_user`, but returns the ids of the sessions reached.
    /// Used for pull requests so each delivered copy can be accounted for.
    pub fn send_to_user_collect(&self, user_id: i64, msg: ServerMsg) -> Vec<u64> {
        let Some(mut sessions) = self.online.get_mut(&user_id) else {
            return Vec::new();
        };
        sessions.retain(|s| !s.tx.is_closed());
        let mut ids = Vec::new();
        for s in sessions.iter() {
            if s.tx.send(msg.clone()).is_ok() {
                ids.push(s.session_id);
            }
        }
        ids
    }

    pub fn note_pull(&self, requester: i64, responder: i64, responder_session: u64) {
        *self
            .pulls
            .entry((requester, responder, responder_session))
            .or_insert(0) += 1;
    }

    /// `true` if a pull answered by this responder session is still open.
    /// Used to validate non-final parts without consuming the pull.
    pub fn has_pull(&self, requester: i64, responder: i64, responder_session: u64) -> bool {
        self.pulls
            .get(&(requester, responder, responder_session))
            .map(|n| *n > 0)
            .unwrap_or(false)
    }

    /// Consumes one outstanding pull; `false` if the response is unsolicited.
    pub fn take_pull(&self, requester: i64, responder: i64, responder_session: u64) -> bool {
        let key = (requester, responder, responder_session);
        let Some(mut n) = self.pulls.get_mut(&key) else {
            return false;
        };
        *n -= 1;
        let empty = *n == 0;
        drop(n);
        if empty {
            self.pulls.remove_if(&key, |_, n| *n == 0);
        }
        true
    }

    /// A responder session went away: its unanswered pulls can never be
    /// answered, so drop them (prevents leaks and late unsolicited pushes).
    pub fn clear_pulls_for_session(&self, user_id: i64, session_id: u64) {
        self.pulls
            .retain(|&(_, responder, sid), _| !(responder == user_id && sid == session_id));
    }

    /// The user is fully gone (last session closed / account deleted):
    /// drop every pull where they are requester OR responder.
    pub fn clear_pulls_for_user(&self, user_id: i64) {
        self.pulls
            .retain(|&(requester, responder, _), _| requester != user_id && responder != user_id);
    }

    /// Adds a new session. Returns `true` if this is the user's first session.
    pub fn add_session(&self, user_id: i64, session: OnlineSession) -> bool {
        let mut entry = self.online.entry(user_id).or_default();
        entry.retain(|s| !s.tx.is_closed());
        let was_empty = entry.is_empty();
        entry.push(session);
        was_empty
    }

    /// Removes a session by id. Returns `true` if no sessions remain.
    pub fn remove_session(&self, user_id: i64, session_id: u64) -> bool {
        let mut empty = false;
        let mut drop_entry = false;
        if let Some(mut e) = self.online.get_mut(&user_id) {
            e.retain(|s| s.session_id != session_id && !s.tx.is_closed());
            empty = e.is_empty();
            drop_entry = empty;
        }
        if drop_entry {
            self.online.remove(&user_id);
        }
        empty
    }
}