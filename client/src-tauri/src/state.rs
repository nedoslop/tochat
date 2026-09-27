use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::crypto::{build_cipher, Cipher, EncryptionConfig};
use crate::db::Database;
use crate::protocol::ClientMsg;

/// Active encryption configuration + cipher instance.
pub struct EncryptionState {
    pub config: EncryptionConfig,
    pub cipher: Option<Box<dyn Cipher>>,
}

impl Default for EncryptionState {
    fn default() -> Self {
        Self {
            config: EncryptionConfig::default(),
            cipher: None,
        }
    }
}

impl EncryptionState {
    pub fn set(&mut self, config: EncryptionConfig) {
        self.cipher = build_cipher(config.method, config.secret.as_deref());
        self.config = config;
    }
}

/// A live WebSocket session: outbound channel plus the two task handles.
///
/// We keep the `JoinHandle`s so we can `abort()` them and `await` their
/// termination on logout / reconnect — that is what makes the logout button
/// deterministic instead of racy.
pub struct WsSession {
    pub id: u64,
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub reader: JoinHandle<()>,
    pub writer: JoinHandle<()>,
}

pub struct AppState {
    pub app: AppHandle,
    pub data_dir: PathBuf,
    /// Per-user local database. `None` while logged out.
    pub db: RwLock<Option<Arc<Database>>>,
    /// Currently logged-in username, if any.
    pub me: RwLock<Option<String>>,
    /// Active WebSocket session, if any.
    pub ws: Mutex<Option<WsSession>>,
    pub encryption: RwLock<EncryptionState>,
    /// Monotonic id source for sessions.
    pub ws_counter: AtomicU64,
}

impl AppState {
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Self {
        // Encryption config is still global for now; per-user config is a
        // natural future extension but was not requested.
        let config = std::fs::read_to_string(encryption_path(&data_dir))
            .ok()
            .and_then(|s| serde_json::from_str::<EncryptionConfig>(&s).ok())
            .unwrap_or_default();

        let mut encryption = EncryptionState::default();
        encryption.set(config);

        Self {
            app,
            data_dir,
            db: RwLock::new(None),
            me: RwLock::new(None),
            ws: Mutex::new(None),
            encryption: RwLock::new(encryption),
            ws_counter: AtomicU64::new(1),
        }
    }

    /// Returns the active per-user database, or an error if the user is not
    /// logged in.
    pub async fn active_db(&self) -> Result<Arc<Database>, String> {
        self.db
            .read()
            .await
            .clone()
            .ok_or_else(|| "not logged in".to_string())
    }

    pub async fn encrypt_for_wire(&self, plaintext: &str) -> Result<String, String> {
        let enc = self.encryption.read().await;
        match enc.cipher.as_ref() {
            Some(c) => c
                .encrypt(plaintext)
                .ok_or_else(|| "encryption failed".to_string()),
            None => Ok(plaintext.to_string()),
        }
    }

    pub async fn decode_from_wire(&self, wire: &str) -> (String, bool) {
        let enc = self.encryption.read().await;
        match enc.cipher.as_ref() {
            Some(c) => match c.decrypt(wire) {
                Some(pt) => (pt, true),
                None => (wire.to_string(), false),
            },
            None => (wire.to_string(), false),
        }
    }
}

pub fn encryption_path(dir: &Path) -> PathBuf {
    dir.join("encryption.json")
}

/// Path of the per-user local message database.
pub fn user_db_path(dir: &Path, username: &str) -> PathBuf {
    // Sanitize the username so it can be used safely as a file name.
    let safe: String = username
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    dir.join(format!("{safe}.db"))
}