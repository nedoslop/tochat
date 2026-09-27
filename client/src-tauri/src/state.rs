use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, Mutex, RwLock};

use crate::db::Database;
use crate::protocol::ClientMsg;

/// In-memory encryption configuration.
///
/// `key` is the derived symmetric key; `password` is kept only so the UI can
/// echo it back. Both are `None` when encryption is off.
#[derive(Default)]
pub struct EncryptionState {
    pub password: Option<String>,
    pub key: Option<[u8; 32]>,
}

impl EncryptionState {
    pub fn is_enabled(&self) -> bool {
        self.key.is_some()
    }
}

pub struct AppState {
    pub app: AppHandle,
    pub data_dir: PathBuf,
    pub db: Database,
    /// Current logged-in username, if any.
    pub me: RwLock<Option<String>>,
    /// Outbound channel for the active WebSocket session.
    pub ws: Mutex<Option<mpsc::UnboundedSender<ClientMsg>>>,
    /// Generation counter used to distinguish "current" from stale readers.
    pub ws_gen: AtomicU64,
    pub encryption: RwLock<EncryptionState>,
}

impl AppState {
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Self {
        let db = Database::open(&data_dir.join("client.db"))
            .expect("failed to open local database");

        // Load the persisted shared password (if any).
        let password = std::fs::read_to_string(encryption_path(&data_dir))
            .ok()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        let key = password.as_deref().map(crate::util::derive_key);

        Self {
            app,
            data_dir,
            db,
            me: RwLock::new(None),
            ws: Mutex::new(None),
            ws_gen: AtomicU64::new(0),
            encryption: RwLock::new(EncryptionState { password, key }),
        }
    }
}

pub fn encryption_path(dir: &std::path::Path) -> PathBuf {
    dir.join("encryption.key")
}