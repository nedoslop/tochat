use std::sync::Arc;

use serde_json::json;
use tauri::State;

use crate::db::LocalMsg;
use crate::protocol::{ClientMsg, KIND_TEXT};
use crate::state::{encryption_path, AppState};
use crate::util::{derive_key, encrypt_payload, now_ms, random_id};
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
/// `plaintext = true`. The wire payload is encrypted when a key is set.
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
    let wire_payload = {
        let enc = state.encryption.read().await;
        match enc.key.as_ref() {
            Some(key) => encrypt_payload(key, plaintext)
                .ok_or_else(|| "encryption failed".to_string())?,
            None => plaintext.to_string(),
        }
    };

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
    // Also drop any stored encryption password.
    let mut enc = state.encryption.write().await;
    enc.password = None;
    enc.key = None;
    drop(enc);
    let _ = std::fs::remove_file(encryption_path(&state.data_dir));
    Ok(())
}

// ---------- encryption ----------

/// Sets (or clears) the shared encryption password.
///
/// Changing this only affects messages sent/received afterwards: existing
/// local rows are left untouched.
#[tauri::command]
pub async fn set_encryption(
    password: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let normalized = password
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty());

    match normalized {
        Some(pw) => {
            let key = derive_key(&pw);
            {
                let mut enc = state.encryption.write().await;
                enc.password = Some(pw.clone());
                enc.key = Some(key);
            }
            std::fs::write(encryption_path(&state.data_dir), pw.as_bytes())
                .map_err(|e| format!("persist error: {e}"))?;
        }
        None => {
            {
                let mut enc = state.encryption.write().await;
                enc.password = None;
                enc.key = None;
            }
            let _ = std::fs::remove_file(encryption_path(&state.data_dir));
        }
    }
    Ok(())
}

/// Returns `true` when encryption is currently active.
#[tauri::command]
pub async fn get_encryption(state: State<'_, Arc<AppState>>) -> Result<bool, String> {
    Ok(state.encryption.read().await.is_enabled())
}