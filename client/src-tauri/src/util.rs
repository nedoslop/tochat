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

/// Decodes `data:<mime>;base64,<payload>` into raw bytes.
/// Shared by the native file-save dialog and native audio playback.
pub fn decode_data_url(url: &str) -> Result<Vec<u8>, String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| "not a data url".to_string())?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "malformed data url".to_string())?;
    if !meta.ends_with(";base64") {
        return Err("only base64 data urls are supported".to_string());
    }
    STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("base64 decode error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pins the KDF output. Both peers must derive identical keys, so any
    /// change to the salt or Argon2 parameters (including a crate upgrade
    /// that changes `Argon2::default()`) silently breaks every existing
    /// shared-password chat. Vector: Argon2id v19, m=19456, t=2, p=1.
    #[test]
    fn derive_key_matches_known_vector() {
        assert_eq!(
            hex::encode(derive_key("topsecret123")),
            "08a1115d9ac7b9c468b4779a5ea2b8cdac56d5b9037958d027aac43cbabb1682"
        );
    }

    #[test]
    fn random_ids_are_unique() {
        assert_ne!(random_id(), random_id());
    }
}
