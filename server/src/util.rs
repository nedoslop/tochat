use sha2::{Digest, Sha256};

/// Hashes a password with SHA-256 (client uses the same).
pub fn hash_password(pw: &str) -> String {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    format!("{:x}", h.finalize())
}

/// Current UNIX time in milliseconds (used as ts / edit_ts).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
