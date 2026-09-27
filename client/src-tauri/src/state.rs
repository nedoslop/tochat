use std::path::PathBuf;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::db::Database;
use crate::protocol::ClientMsg;

/// Handle to the active WebSocket connection.
pub struct WsHandle {
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub task: tokio::task::JoinHandle<()>,
}

/// Shared client application state.
pub struct AppState {
    pub app: AppHandle,
    pub data_dir: PathBuf,
    pub db: RwLock<Option<Arc<Database>>>,
    pub me: RwLock<Option<String>>,
    pub ws: Mutex<Option<WsHandle>>,
}

impl AppState {
    /// Creates a new state value.
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Self {
        Self {
            app,
            data_dir,
            db: RwLock::new(None),
            me: RwLock::new(None),
            ws: Mutex::new(None),
        }
    }

    /// Returns the current user's DB, or an error if not logged in.
    pub async fn db(&self) -> Result<Arc<Database>, String> {
        self.db
            .read()
            .await
            .clone()
            .ok_or_else(|| "not logged in".to_string())
    }

    /// Returns the current username, or an error if not logged in.
    pub async fn me(&self) -> Result<String, String> {
        self.me
            .read()
            .await
            .clone()
            .ok_or_else(|| "not logged in".to_string())
    }

    /// Sends a ClientMsg through the active WebSocket, if any.
    pub async fn send(&self, msg: ClientMsg) -> Result<(), String> {
        let guard = self.ws.lock().await;
        let handle = guard.as_ref().ok_or("not connected")?;
        handle.tx.send(msg).map_err(|_| "connection closed".into())
    }
}