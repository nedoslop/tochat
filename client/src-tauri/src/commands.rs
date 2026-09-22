use std::sync::Arc;

use tauri::{AppHandle, State};

use crate::crypto;
use crate::http_auth;
use crate::protocol::ClientMsg;
use crate::state::AppState;
use crate::util::now;
use crate::ws;

#[tauri::command]
pub async fn register(base_url: String, username: String, password: String) -> Result<(), String> {
    http_auth::register(&base_url, &username, &password).await
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    ws::connect(app, state.inner().clone(), base_url, username, password).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut ws = state.ws.lock().await;
    *ws = None;
    Ok(())
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    to: String,
    text: String,
) -> Result<(), String> {
    let ts = now();
    let (method, pw) = state.db.get_peer_encryption(&to).await;
    let payload = if method == "aes-gcm" {
        if let Some(pw) = pw {
            crypto::encrypt(&pw, &text)
        } else {
            text.clone()
        }
    } else {
        text.clone()
    };
    state.db.insert_message(&to, "out", ts, &text).await;

    let ws = state.ws.lock().await;
    if let Some(handle) = ws.as_ref() {
        handle
            .tx
            .send(ClientMsg::Send { to, payload })
            .map_err(|e| e.to_string())?;
    } else {
        return Err("not connected".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn pull_history(
    state: State<'_, Arc<AppState>>,
    from: String,
    since: Option<i64>,
) -> Result<(), String> {
    let since = since.unwrap_or_else(|| 0);
    let ws = state.ws.lock().await;
    if let Some(handle) = ws.as_ref() {
        handle
            .tx
            .send(ClientMsg::PullHistory { from, since })
            .map_err(|e| e.to_string())?;
    } else {
        return Err("not connected".into());
    }
    Ok(())
}

#[tauri::command]
pub async fn list_pending(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let ws = state.ws.lock().await;
    if let Some(handle) = ws.as_ref() {
        handle
            .tx
            .send(ClientMsg::ListPending)
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn delete_account(
    state: State<'_, Arc<AppState>>,
    password: String,
) -> Result<(), String> {
    let ws = state.ws.lock().await;
    if let Some(handle) = ws.as_ref() {
        handle
            .tx
            .send(ClientMsg::DeleteAccount { password })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
pub async fn set_peer_encryption(
    state: State<'_, Arc<AppState>>,
    peer: String,
    method: String,
    password: Option<String>,
) -> Result<(), String> {
    state
        .db
        .set_peer_encryption(&peer, &method, password.as_deref())
        .await;
    Ok(())
}

#[tauri::command]
pub async fn get_peer_encryption(
    state: State<'_, Arc<AppState>>,
    peer: String,
) -> Result<serde_json::Value, String> {
    let (method, _) = state.db.get_peer_encryption(&peer).await;
    Ok(serde_json::json!({ "method": method }))
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, Arc<AppState>>,
    peer: String,
) -> Result<Vec<crate::db::Message>, String> {
    Ok(state.db.get_messages(&peer).await)
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.db.wipe().await;
    Ok(())
}