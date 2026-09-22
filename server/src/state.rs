use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::db::Database;
use crate::protocol::ServerMsg;

#[derive(Clone)]
pub struct OnlineSession {
    pub tx: mpsc::UnboundedSender<ServerMsg>,
    pub session_id: u64,
}

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    /// Active websocket connections keyed by username.
    pub online: Arc<DashMap<String, OnlineSession>>,
}