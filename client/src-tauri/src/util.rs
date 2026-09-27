use sha2::{Digest, Sha256};

/// Hashes a password with SHA-256 (matches the server).
pub fn hash_password(pw: &str) -> String {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    format!("{:x}", h.finalize())
}

/// Current UNIX time in milliseconds.
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// Generates a random 128-bit message id (hex).
pub fn new_msg_id() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let a: u64 = rng.gen();
    let b: u64 = rng.gen();
    format!("{:016x}{:016x}", a, b)
}

/// Hex-encodes a username for use as a safe filename component.
pub fn encode_username(username: &str) -> String {
    username.bytes().map(|b| format!("{:02x}", b)).collect()
}