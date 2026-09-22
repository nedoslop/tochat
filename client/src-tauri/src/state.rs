use std::sync::atomic::{AtomicU64, Ordering};

use tauri::{AppHandle, Manager};
use tokio::sync::{mpsc, Mutex};

use crate::db::Database;
use crate::protocol::ClientMsg;

pub struct WsHandle {
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub session_id: u64,
}

pub struct AppState {
    pub db: Database,
    pub ws: Mutex<Option<WsHandle>>,
    pub username: Mutex<Option<String>>,
    /// Monotonic id of the currently active websocket session. Any
    /// reader/writer task whose own id doesn't match this is considered
    /// stale (e.g. after a page reload + re-login) and must not emit UI
    /// events or mutate shared state.
    pub current_session: AtomicU64,
    pub app: AppHandle,
}

impl AppState {
    pub fn new(app: AppHandle) -> Result<Self, String> {
        let db_path = app
            .path()
            .app_data_dir()
            .map_err(|e| e.to_string())?
            .join("chat.db");
        let db = Database::open(db_path.to_str().unwrap()).map_err(|e| e.to_string())?;
        Ok(Self {
            db,
            ws: Mutex::new(None),
            username: Mutex::new(None),
            current_session: AtomicU64::new(0),
            app,
        })
    }

    pub fn is_current_session(&self, id: u64) -> bool {
        self.current_session.load(Ordering::Relaxed) == id
    }

    /// The currently logged-in account name. All local DB access is scoped
    /// by this so multiple accounts (even in parallel app instances sharing
    /// the same SQLite file) never mix history.
    pub async fn owner(&self) -> Result<String, String> {
        self.username
            .lock()
            .await
            .clone()
            .ok_or_else(|| "not logged in".to_string())
    }
}