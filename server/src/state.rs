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
    pub online: Arc<DashMap<i64, OnlineSession>>,
}