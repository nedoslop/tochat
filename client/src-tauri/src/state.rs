use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, oneshot, Mutex, RwLock};
use tokio::task::JoinHandle;

use crate::crypto::{build_cipher, Cipher, EncryptionConfig, EncryptionMethod};
use crate::db::Database;
use crate::protocol::{ClientMsg, UserStatus};

/// Per-peer encryption: config + cached cipher.
///
/// Encryption in this app is end-to-end: each chat is its own trust
/// domain with its own key. The server never sees keys and never sees
/// plaintext. There is deliberately no global key.
pub struct EncryptionMap {
    configs: HashMap<i64, EncryptionConfig>,
    ciphers: HashMap<i64, Box<dyn Cipher>>,
}

impl EncryptionMap {
    pub fn new() -> Self {
        Self {
            configs: HashMap::new(),
            ciphers: HashMap::new(),
        }
    }

    pub fn clear(&mut self) {
        self.configs.clear();
        self.ciphers.clear();
    }

    /// Sets the config for `peer_id`. `method = None` removes the entry.
    pub fn set(&mut self, peer_id: i64, config: EncryptionConfig) {
        self.ciphers.remove(&peer_id);
        if config.method == EncryptionMethod::None {
            self.configs.remove(&peer_id);
            return;
        }
        if let Some(c) = build_cipher(config.method, config.secret.as_deref()) {
            self.ciphers.insert(peer_id, c);
        }
        // Keep the config even if the cipher couldn't be built, so the
        // UI can surface "secret is set but invalid" states.
        self.configs.insert(peer_id, config);
    }

    pub fn get_config(&self, peer_id: i64) -> EncryptionConfig {
        self.configs.get(&peer_id).cloned().unwrap_or_default()
    }

    pub fn cipher(&self, peer_id: i64) -> Option<&dyn Cipher> {
        self.ciphers.get(&peer_id).map(|b| b.as_ref())
    }
}

impl Default for EncryptionMap {
    fn default() -> Self {
        Self::new()
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
    /// Per-peer encryption state (in memory; persisted in the local DB).
    pub encryption: RwLock<EncryptionMap>,
    pub my_status: RwLock<UserStatus>,
    pub ws_counter: AtomicU64,
    /// In-flight `resolve_user` lookups keyed by username.
    pub pending_lookups: Mutex<HashMap<String, oneshot::Sender<Option<i64>>>>,
}

impl AppState {
    pub fn new(app: AppHandle, data_dir: PathBuf) -> Self {
        Self {
            app,
            data_dir,
            server_url: RwLock::new(None),
            db: RwLock::new(None),
            me: RwLock::new(None),
            ws: Mutex::new(None),
            encryption: RwLock::new(EncryptionMap::new()),
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

    /// Loads every persisted per-peer encryption config from the DB into
    /// memory. Called once after a successful login (the DB is per-user,
    /// per-server, so switching servers/users reloads from scratch).
    pub async fn load_encryption_from_db(&self) -> Result<(), String> {
        let db = self.active_db().await?;
        let rows = db.load_all_encryption().await;
        let mut map = self.encryption.write().await;
        map.clear();
        for (peer_id, method_str, secret) in rows {
            let method = EncryptionMethod::parse(&method_str);
            map.set(peer_id, EncryptionConfig { method, secret });
        }
        Ok(())
    }

    pub async fn encrypt_for_wire(
        &self,
        peer_id: i64,
        plaintext: &str,
    ) -> Result<String, String> {
        let map = self.encryption.read().await;
        match map.cipher(peer_id) {
            Some(c) => c
                .encrypt(plaintext)
                .ok_or_else(|| "encryption failed".to_string()),
            None => Ok(plaintext.to_string()),
        }
    }

    pub async fn decode_from_wire(&self, peer_id: i64, wire: &str) -> (String, bool) {
        let map = self.encryption.read().await;
        match map.cipher(peer_id) {
            Some(c) => match c.decrypt(wire) {
                Some(pt) => (pt, true),
                None => (wire.to_string(), false),
            },
            None => (wire.to_string(), false),
        }
    }
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