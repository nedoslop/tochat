use argon2::Argon2;
use base64::{engine::general_purpose::STANDARD, Engine};
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

/// Derives a 32-byte symmetric key from a shared password.
pub fn derive_key(password: &str) -> [u8; 32] {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(password.as_bytes(), SALT, &mut key)
        .expect("argon2 key derivation");
    key
}
