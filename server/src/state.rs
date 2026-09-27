use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::db::Database;
use crate::protocol::ServerMsg;

/// One active WebSocket session belonging to a logged-in user.
#[derive(Clone)]
pub struct OnlineSession {
    pub tx: mpsc::UnboundedSender<ServerMsg>,
    pub session_id: u64,
}

/// Shared application state.
#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    // TODO: store users by ID instead of names
    /// Online sessions keyed by username.
    pub online: Arc<DashMap<String, OnlineSession>>,
}
