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
            app,
        })
    }
}