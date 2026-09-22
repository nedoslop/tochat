use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use sha2::{Digest, Sha256};

pub fn encrypt(password: &str, plaintext: &str) -> String {
    let key = Sha256::digest(password.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(&key).unwrap();
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ct = cipher.encrypt(&nonce, plaintext.as_bytes()).unwrap();
    let mut out = nonce.to_vec();
    out.extend_from_slice(&ct);
    STANDARD.encode(out)
}

pub fn decrypt(password: &str, data: &str) -> Option<String> {
    let bytes = STANDARD.decode(data).ok()?;
    if bytes.len() < 12 {
        return None;
    }
    let (nonce, ct) = bytes.split_at(12);
    let key = Sha256::digest(password.as_bytes());
    let cipher = Aes256Gcm::new_from_slice(&key).ok()?;
    let nonce = Nonce::from_slice(nonce);
    let pt = cipher.decrypt(nonce, ct).ok()?;
    String::from_utf8(pt).ok()
}