//! Client-side payload encryption framework.
//!
//! Adding a new method is deliberately mechanical:
//!   1. add a variant to [`EncryptionMethod`],
//!   2. implement [`Cipher`] for it,
//!   3. add one arm in [`build_cipher`].

use serde::{Deserialize, Serialize};

use crate::util::derive_key;

/// Supported payload encryption methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum EncryptionMethod {
    /// Payloads are sent as plaintext.
    #[default]
    None,
    /// Argon2-KDF over a shared password, then ChaCha20-Poly1305 per message.
    SharedPassword,
}

impl EncryptionMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::SharedPassword => "shared_password",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "shared_password" => Self::SharedPassword,
            _ => Self::None,
        }
    }
}

/// Persisted encryption configuration (written to `encryption.json`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionConfig {
    pub method: EncryptionMethod,
    #[serde(default)]
    pub secret: Option<String>,
}

impl Default for EncryptionConfig {
    fn default() -> Self {
        Self {
            method: EncryptionMethod::None,
            secret: None,
        }
    }
}

/// Common interface for every encryption method.
///
/// `encrypt` takes a UTF-8 plaintext payload and returns the on-the-wire
/// string. `decrypt` is the inverse; it must return `None` on any failure
/// (wrong key, malformed input, non-UTF-8) so callers can store the raw
/// value and surface it as-is.
pub trait Cipher: Send + Sync {
    fn encrypt(&self, plaintext: &str) -> Option<String>;
    fn decrypt(&self, wire: &str) -> Option<String>;
}

// ---------- shared-password method ------------------------------------------

/// Argon2(password) -> 32-byte key; per-message ChaCha20-Poly1305 with a
/// random 12-byte nonce. Wire format: `base64(nonce || ciphertext+tag)`.
pub struct SharedPasswordCipher {
    key: [u8; 32],
}

impl SharedPasswordCipher {
    pub fn new(password: &str) -> Self {
        Self {
            key: derive_key(password),
        }
    }
}

impl Cipher for SharedPasswordCipher {
    fn encrypt(&self, plaintext: &str) -> Option<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use chacha20poly1305::{
            aead::{Aead, KeyInit},
            ChaCha20Poly1305, Nonce,
        };
        use rand::RngCore;

        let cipher = ChaCha20Poly1305::new_from_slice(&self.key).ok()?;
        let mut nonce_bytes = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
        let ct = cipher
            .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
            .ok()?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ct);
        Some(STANDARD.encode(&out))
    }

    fn decrypt(&self, wire: &str) -> Option<String> {
        use base64::{engine::general_purpose::STANDARD, Engine};
        use chacha20poly1305::{
            aead::{Aead, KeyInit},
            ChaCha20Poly1305, Nonce,
        };

        let data = STANDARD.decode(wire.trim()).ok()?;
        if data.len() < 12 {
            return None;
        }
        let (nonce_bytes, ct) = data.split_at(12);
        let cipher = ChaCha20Poly1305::new_from_slice(&self.key).ok()?;
        let pt = cipher.decrypt(Nonce::from_slice(nonce_bytes), ct).ok()?;
        String::from_utf8(pt).ok()
    }
}

// ---------- factory ---------------------------------------------------------

/// Instantiates the cipher for a given method + secret.
///
/// Returns `None` when the method is `None` or the secret is missing/empty;
/// callers treat that as "no encryption".
pub fn build_cipher(method: EncryptionMethod, secret: Option<&str>) -> Option<Box<dyn Cipher>> {
    match method {
        EncryptionMethod::None => None,
        EncryptionMethod::SharedPassword => {
            let pw = secret?;
            if pw.is_empty() {
                return None;
            }
            Some(Box::new(SharedPasswordCipher::new(pw)))
        }
    }
}