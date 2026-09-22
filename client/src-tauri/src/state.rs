use tokio::sync::{mpsc, Mutex};

use crate::db::Database;
use crate::protocol::ClientMsg;

pub struct AppState {
    pub db: Database,
    pub ws: Mutex<WsSession>,
}

/// The currently open websocket session, if any.
#[derive(Default)]
pub struct WsSession {
    pub tx: Option<mpsc::UnboundedSender<ClientMsg>>,
    pub username: Option<String>,
    /// Server-side timestamp of our previous disconnect.
    /// Used as the default `since` for history pulls.
    pub last_seen: i64,
}

impl AppState {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            ws: Mutex::new(WsSession::default()),
        }
    }
}
