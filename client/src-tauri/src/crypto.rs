use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::{engine::general_purpose::STANDARD, Engine};
use rand::RngCore;
use sha2::{Digest, Sha256};

const PREFIX: &str = "enc:aes-gcm:";

fn derive_key(password: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(password.as_bytes());
    let out = h.finalize();
    let mut key = [0u8; 32];
    key.copy_from_slice(&out);
    key
}

pub fn encrypt(method: &str, password: Option<&str>, plaintext: &str) -> Result<String, String> {
    match method {
        "none" => Ok(plaintext.to_string()),
        "aes-gcm" => {
            let pw = password.ok_or("password required for aes-gcm")?;
            let key = derive_key(pw);
            let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("key: {e}"))?;

            let mut nonce_bytes = [0u8; 12];
            rand::thread_rng().fill_bytes(&mut nonce_bytes);
            let nonce = Nonce::from_slice(&nonce_bytes);

            let ct = cipher
                .encrypt(nonce, plaintext.as_bytes())
                .map_err(|e| format!("encrypt: {e}"))?;

            let mut combined = Vec::with_capacity(12 + ct.len());
            combined.extend_from_slice(&nonce_bytes);
            combined.extend_from_slice(&ct);

            Ok(format!("{PREFIX}{}", STANDARD.encode(&combined)))
        }
        other => Err(format!("unknown method: {other}")),
    }
}

/// Decryption is driven by the payload's prefix — the method saved in the
/// local DB is only used when *encrypting*.
pub fn decrypt(payload: &str, password: Option<&str>) -> Result<String, String> {
    if let Some(b64) = payload.strip_prefix(PREFIX) {
        let pw = password.ok_or("password required for aes-gcm")?;
        let combined = STANDARD.decode(b64).map_err(|e| format!("b64: {e}"))?;
        if combined.len() < 12 {
            return Err("payload too short".into());
        }
        let (nonce_bytes, ct) = combined.split_at(12);
        let key = derive_key(pw);
        let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| format!("key: {e}"))?;
        let nonce = Nonce::from_slice(nonce_bytes);
        let pt = cipher
            .decrypt(nonce, ct)
            .map_err(|e| format!("decrypt: {e}"))?;
        String::from_utf8(pt).map_err(|e| format!("utf8: {e}"))
    } else {
        Ok(payload.to_string())
    }
}
