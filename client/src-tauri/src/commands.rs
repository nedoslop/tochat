use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use tauri::State;

use crate::crypto::{EncryptionConfig, EncryptionMethod};
use crate::db::LocalMsg;
use crate::protocol::{ClientMsg, KIND_TEXT};
use crate::state::{encryption_path, AppState};
use crate::util::{now_ms, random_id};
use crate::ws;

// ---------- auth ----------

#[tauri::command]
pub async fn register(
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim().trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client
        .post(&url)
        .json(&json!({ "username": username, "password": password }))
        .send()
        .await
        .map_err(|e| format!("network error: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(format!("register failed ({status}): {text}"))
    }
}

#[tauri::command]
pub async fn connect(
    base_url: String,
    username: String,
    password: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let st = state.inner().clone();
    ws::connect(st, base_url, username, password).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut ws = state.ws.lock().await;
    *ws = None;
    drop(ws);
    *state.me.write().await = None;
    Ok(())
}

// ---------- messaging ----------

#[tauri::command]
pub async fn send_message(
    to: String,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if text.is_empty() {
        return Err("empty message".into());
    }
    let id = random_id();
    let ts = now_ms();
    send_outgoing(&state, &to, &id, ts, ts, &text, false).await
}

#[tauri::command]
pub async fn edit_message(
    peer: String,
    id: String,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if text.is_empty() {
        return Err("empty message".into());
    }
    let ts = state
        .db
        .get_messages(&peer)
        .await
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.ts)
        .unwrap_or_else(now_ms);
    let edit_ts = now_ms();
    send_outgoing(&state, &peer, &id, ts, edit_ts, &text, true).await
}

#[tauri::command]
pub async fn delete_message(
    peer: String,
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let ts = state
        .db
        .get_messages(&peer)
        .await
        .iter()
        .find(|m| m.id == id)
        .map(|m| m.ts)
        .unwrap_or_else(now_ms);
    let edit_ts = now_ms();
    send_outgoing(&state, &peer, &id, ts, edit_ts, "", true).await
}

/// Shared logic for new messages and edits/deletes.
///
/// Locally the plaintext (or empty, for deletes) is stored with
/// `plaintext = true`. The wire payload is encrypted if a cipher is active.
#[allow(clippy::too_many_arguments)]
async fn send_outgoing(
    state: &Arc<AppState>,
    to: &str,
    id: &str,
    ts: i64,
    edit_ts: i64,
    plaintext: &str,
    is_edit: bool,
) -> Result<(), String> {
    let wire_payload = state.encrypt_for_wire(plaintext).await?;

    state
        .db
        .upsert_message(to, id, "out", ts, edit_ts, KIND_TEXT, plaintext, true)
        .await;

    let tx = {
        let ws = state.ws.lock().await;
        ws.clone().ok_or_else(|| "not connected".to_string())?
    };

    let msg = if is_edit {
        ClientMsg::Edit {
            to: to.to_string(),
            id: id.to_string(),
            ts,
            edit_ts,
            kind: KIND_TEXT.to_string(),
            payload: wire_payload,
        }
    } else {
        ClientMsg::Send {
            to: to.to_string(),
            id: id.to_string(),
            ts,
            kind: KIND_TEXT.to_string(),
            payload: wire_payload,
        }
    };

    tx.send(msg).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn pull_history(
    from: String,
    since: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.clone().ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::PullHistory { from, since })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_pending(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.clone().ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::ListPending)
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_account(
    password: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.clone().ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::DeleteAccount { password })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

// ---------- local storage ----------

#[tauri::command]
pub async fn get_messages(
    peer: String,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LocalMsg>, String> {
    Ok(state.db.get_messages(&peer).await)
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.db.wipe_all().await;

    let config = EncryptionConfig::default();
    {
        let mut enc = state.encryption.write().await;
        enc.set(config);
    }
    let _ = std::fs::remove_file(encryption_path(&state.data_dir));
    Ok(())
}

// ---------- encryption ----------

/// Snapshot of the current encryption configuration, safe to expose to the UI.
#[derive(Serialize)]
pub struct EncryptionInfo {
    /// One of `"none"`, `"shared_password"`.
    pub method: String,
    /// Whether a secret is currently stored for the active method.
    pub has_secret: bool,
}

#[tauri::command]
pub async fn get_encryption(
    state: State<'_, Arc<AppState>>,
) -> Result<EncryptionInfo, String> {
    let enc = state.encryption.read().await;
    Ok(EncryptionInfo {
        method: enc.config.method.as_str().to_string(),
        has_secret: enc
            .config
            .secret
            .as_deref()
            .map_or(false, |s| !s.is_empty()),
    })
}

/// Sets the encryption method + optional secret. Pass `secret = None` with an
/// unchanged method to keep the previously stored secret.
#[tauri::command]
pub async fn set_encryption(
    method: String,
    secret: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let m = EncryptionMethod::parse(&method);
    let mut secret = secret
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());

    // If the caller didn't supply a secret and the method is unchanged, keep
    // the existing one (so "Apply" without retyping the password is a no-op).
    if secret.is_none() {
        let enc = state.encryption.read().await;
        if enc.config.method == m {
            secret = enc.config.secret.clone();
        }
    }

    if m == EncryptionMethod::SharedPassword && secret.is_none() {
        return Err("shared password is required".into());
    }

    let config = EncryptionConfig {
        method: m,
        secret: secret.clone(),
    };

    // Persist before swapping the runtime state so a disk failure doesn't
    // silently change behavior for the running session.
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    std::fs::write(encryption_path(&state.data_dir), json)
        .map_err(|e| format!("persist error: {e}"))?;

    let mut enc = state.encryption.write().await;
    enc.set(config);
    Ok(())
}