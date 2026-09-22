use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::Engine;
use rand::RngCore;
use sha2::{Digest, Sha256};

fn derive_key(password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    let out = h.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    key
}

pub fn encrypt(plaintext: &[u8], password: &str) -> Result<String, String> {
    let key = derive_key(password);
    let cipher = Aes256Gcm::new(&key.into());
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plaintext).map_err(|e| e.to_string())?;
    let mut combined = Vec::with_capacity(12 + ct.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ct);
    Ok(base64::engine::general_purpose::STANDARD.encode(combined))
}

pub fn decrypt(encoded: &str, password: &str) -> Result<Vec<u8>, String> {
    let data = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| e.to_string())?;
    if data.len() < 12 {
        return Err("ciphertext too short".into());
    }
    let (nonce_bytes, ct) = data.split_at(12);
    let key = derive_key(password);
    let cipher = Aes256Gcm::new(&key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ct)
        .map_err(|e| e.to_string())
}