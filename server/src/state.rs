use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::mpsc;

use crate::db::Database;
use crate::protocol::ServerMsg;

#[derive(Clone)]
pub struct AppState {
    pub db: Database,
    /// Active websocket connections keyed by username.
    pub online: Arc<DashMap<String, mpsc::UnboundedSender<ServerMsg>>>,
}