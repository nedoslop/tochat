use sha2::{Digest, Sha256};
use std::fmt::Write as _;

/// Hashes a password with SHA-256 (client uses the same).
pub fn hash_password(pw: &str) -> String {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    // sha2 0.11 (digest 0.11) returns a hybrid-array `Array`, which no
    // longer implements `LowerHex` (RustCrypto/hybrid-array#201). Format
    // the bytes ourselves so the stored hash stays byte-for-byte identical
    // to the old `format!("{:x}", ..)` output: 64 lowercase hex chars.
    let digest = h.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest.iter() {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Current UNIX time in milliseconds (used as ts / edit_ts).
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
