use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

fn key_from_password(pw: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    let r = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&r);
    k
}

pub fn encrypt(pw: &str, plaintext: &str) -> String {
    let key = key_from_password(pw);
    let cipher = Aes256Gcm::new((&key).into());
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plaintext.as_bytes()).unwrap();
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    B64.encode(&out)
}

pub fn decrypt(pw: &str, payload: &str) -> Option<String> {
    let key = key_from_password(pw);
    let cipher = Aes256Gcm::new((&key).into());
    let data = B64.decode(payload).ok()?;
    if data.len() < 12 {
        return None;
    }
    let (nonce_b, ct) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_b);
    let pt = cipher.decrypt(nonce, ct).ok()?;
    String::from_utf8(pt).ok()
}