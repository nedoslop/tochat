use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, KeyInit},
    ChaCha20Poly1305, Nonce,
};
use rand::RngCore;

/// Fixed salt for the shared-password KDF. Both peers must derive the same
/// key from the same password, so the salt is a compile-time constant.
const SALT: &[u8] = b"chat-app-shared-encryption-salt-v1";

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Random 16-byte id, base64-encoded.
pub fn random_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    STANDARD.encode(bytes)
}

/// Derives a 32-byte symmetric key from the shared password.
pub fn derive_key(password: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), SALT, &mut key)
        .expect("argon2 key derivation");
    key
}

/// Encrypts `plaintext`, returns base64(nonce || ciphertext+tag).
pub fn encrypt_payload(key: &[u8; 32], plaintext: &str) -> Option<String> {
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    let mut nonce_bytes = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce_bytes);
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce_bytes), plaintext.as_bytes())
        .ok()?;
    let mut combined = Vec::with_capacity(12 + ct.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ct);
    Some(STANDARD.encode(&combined))
}

/// Attempts to decrypt a base64(nonce || ciphertext) payload.
/// Returns `None` on any failure (wrong key, malformed input, non-UTF-8).
pub fn decrypt_payload(key: &[u8; 32], encoded: &str) -> Option<String> {
    let data = STANDARD.decode(encoded.trim()).ok()?;
    if data.len() < 12 {
        return None;
    }
    let (nonce_bytes, ct) = data.split_at(12);
    let cipher = ChaCha20Poly1305::new_from_slice(key).ok()?;
    let pt = cipher.decrypt(Nonce::from_slice(nonce_bytes), ct).ok()?;
    String::from_utf8(pt).ok()
}