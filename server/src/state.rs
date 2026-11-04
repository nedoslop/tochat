use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::db::Database;
use crate::protocol::{ServerMsg, UserStatus};

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
    /// Last known status per user. Persists across session disconnects so a
    /// user who was Busy stays Busy after reconnecting — without relying on
    /// the client to re-announce it. Cleared only on process restart.
    pub user_status: Arc<DashMap<i64, UserStatus>>,
    /// Outstanding history pulls, keyed by `(requester, responder)` with the
    /// number of forwarded requests not yet answered. A `history_response`
    /// is only relayed if it answers one of these, so nobody can push
    /// messages into another user's chat unprompted.
    pub pulls: Arc<DashMap<(i64, i64), u32>>,
}

impl AppState {
    pub fn is_online(&self, user_id: i64) -> bool {
        self.online
            .get(&user_id)
            .map(|v| v.iter().any(|s| !s.tx.is_closed()))
            .unwrap_or(false)
    }

    /// Returns the current status for `user_id`, defaulting to Online.
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
    pub fn send_to_user_except(
        &self,
        user_id: i64,
        except_session: u64,
        msg: ServerMsg,
    ) -> usize {
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

    pub fn note_pull(&self, requester: i64, responder: i64) {
        *self.pulls.entry((requester, responder)).or_insert(0) += 1;
    }

    /// Consumes one outstanding pull; `false` if the response is unsolicited.
    pub fn take_pull(&self, requester: i64, responder: i64) -> bool {
        let Some(mut n) = self.pulls.get_mut(&(requester, responder)) else {
            return false;
        };
        *n -= 1;
        let empty = *n == 0;
        drop(n);
        if empty {
            self.pulls.remove_if(&(requester, responder), |_, n| *n == 0);
        }
        true
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
