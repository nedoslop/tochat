use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::crypto::{build_cipher, Cipher, EncryptionConfig};
use crate::db::Database;
use crate::protocol::{ClientMsg, UserStatus};

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

pub struct WsSession {
    pub id: u64,
    pub tx: mpsc::UnboundedSender<ClientMsg>,
    pub reader: JoinHandle<()>,
    pub writer: JoinHandle<()>,
}

/// The currently logged-in user (id + username).
#[derive(Clone)]
pub struct Me {
    pub id: i64,
    pub username: String,
}

pub struct AppState {
    pub app: AppHandle,
    pub data_dir: PathBuf,
    /// Base URL of the currently-connected server (used to pick DB file).
    pub server_url: RwLock<Option<String>>,
    pub db: RwLock<Option<Arc<Database>>>,
    pub me: RwLock<Option<Me>>,
    pub ws: Mutex<Option<WsSession>>,
    pub encryption: RwLock<EncryptionState>,
    /// My current status.
    pub my_status: RwLock<UserStatus>,
    pub ws_counter: AtomicU64,
    /// In-flight `resolve_user` lookups keyed by username. When a
    /// `ServerMsg::Profile` (or matching error) arrives, the sender is
    /// consumed with the resolved ID (or `None`).
    pub pending_lookups: Mutex<HashMap<String, oneshot::Sender<Option<i64>>>>,
}

impl AppState {
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Self {
        let config = std::fs::read_to_string(encryption_path(&data_dir))
            .ok()
            .and_then(|s| serde_json::from_str::<EncryptionConfig>(&s).ok())
            .unwrap_or_default();

        let mut encryption = EncryptionState::default();
        encryption.set(config);

        Self {
            app,
            data_dir,
            server_url: RwLock::new(None),
            db: RwLock::new(None),
            me: RwLock::new(None),
            ws: Mutex::new(None),
            encryption: RwLock::new(encryption),
            my_status: RwLock::new(UserStatus::Online),
            ws_counter: AtomicU64::new(1),
            pending_lookups: Mutex::new(HashMap::new()),
        }
    }

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

pub fn theme_path(dir: &Path) -> PathBuf {
    dir.join("theme.json")
}

pub fn read_theme(dir: &Path) -> String {
    let raw = std::fs::read_to_string(theme_path(dir)).unwrap_or_default();
    let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_default();
    let theme = parsed
        .get("theme")
        .and_then(|v| v.as_str())
        .unwrap_or("system");
    if matches!(theme, "system" | "light" | "dark") {
        theme.to_string()
    } else {
        "system".to_string()
    }
}

/// Sanitizes a string so it can be used as a file-name component.
fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// FNV-1a hash of the server URL; used to namespace per-server DB files.
fn server_key(url: &str) -> String {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in url.trim().trim_end_matches('/').bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    format!("{:016x}", h)
}

/// Path of the per-user, per-server local message database.
pub fn user_db_path(dir: &Path, username: &str, server_url: &str) -> PathBuf {
    let u = sanitize(username);
    let s = server_key(server_url);
    dir.join(format!("{u}_{s}.db"))
}