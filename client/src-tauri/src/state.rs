use std::path::PathBuf;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;

use tauri::AppHandle;
use tokio::sync::{mpsc, Mutex, RwLock};

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
    pub fn is_enabled(&self) -> bool {
        self.cipher.is_some()
    }

    pub fn set(&mut self, config: EncryptionConfig) {
        self.cipher = build_cipher(config.method, config.secret.as_deref());
        self.config = config;
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

        // Load persisted encryption config (if any).
        let config = std::fs::read_to_string(encryption_path(&data_dir))
            .ok()
            .and_then(|s| serde_json::from_str::<EncryptionConfig>(&s).ok())
            .unwrap_or_default();

        let mut encryption = EncryptionState::default();
        encryption.set(config);

        Self {
            app,
            data_dir,
            db,
            me: RwLock::new(None),
            ws: Mutex::new(None),
            ws_gen: AtomicU64::new(0),
            encryption: RwLock::new(encryption),
        }
    }

    /// Encrypts a plaintext payload for the wire.
    ///
    /// `Ok(payload)` on success; `Err` only if encryption is active but the
    /// cipher itself failed (which callers treat as a hard error). With the
    /// method `None` the plaintext is passed through unchanged.
    pub async fn encrypt_for_wire(&self, plaintext: &str) -> Result<String, String> {
        let enc = self.encryption.read().await;
        match enc.cipher.as_ref() {
            Some(c) => c
                .encrypt(plaintext)
                .ok_or_else(|| "encryption failed".to_string()),
            None => Ok(plaintext.to_string()),
        }
    }

    /// Re-encodes stored plaintext for the wire (used when serving a history
    /// pull). Unlike [`encrypt_for_wire`], failures fall back to the raw text.
    pub async fn reencode_for_wire(&self, plaintext: &str) -> String {
        let enc = self.encryption.read().await;
        match enc.cipher.as_ref() {
            Some(c) => c.encrypt(plaintext).unwrap_or_else(|| plaintext.to_string()),
            None => plaintext.to_string(),
        }
    }

    /// Decodes a wire payload. Returns `(stored_text, is_plaintext)`.
    ///
    /// When decryption fails (wrong key, plaintext sent under an active
    /// cipher, malformed input, or encryption disabled), the raw wire string
    /// is kept and `is_plaintext` is `false` — the local DB then treats it
    /// as opaque.
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

pub fn encryption_path(dir: &std::path::Path) -> PathBuf {
    dir.join("encryption.json")
}