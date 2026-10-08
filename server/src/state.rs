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
}

impl AppState {
    pub fn is_online(&self, user_id: i64) -> bool {
        self.online
            .get(&user_id)
            .map(|v| !v.is_empty())
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
        let Some(sessions) = self.online.get(&user_id) else {
            return 0;
        };
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
        let Some(sessions) = self.online.get(&user_id) else {
            return 0;
        };
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

    /// Adds a new session. Returns `true` if this is the user's first session.
    pub fn add_session(&self, user_id: i64, session: OnlineSession) -> bool {
        let mut entry = self.online.entry(user_id).or_default();
        let was_empty = entry.is_empty();
        entry.push(session);
        was_empty
    }

    /// Removes a session by id. Returns `true` if no sessions remain.
    pub fn remove_session(&self, user_id: i64, session_id: u64) -> bool {
        let mut empty = false;
        let mut drop_entry = false;
        if let Some(mut e) = self.online.get_mut(&user_id) {
            e.retain(|s| s.session_id != session_id);
            empty = e.is_empty();
            drop_entry = empty;
        }
        if drop_entry {
            self.online.remove(&user_id);
        }
        empty
    }
}