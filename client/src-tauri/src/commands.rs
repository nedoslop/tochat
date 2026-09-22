use std::sync::atomic::Ordering;
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
    // Invalidate the current session id so any still-running reader/writer
    // tasks from the old connection immediately stop emitting events into
    // the UI. Their sockets will close on their own once their tx / rx
    // halves are dropped.
    state.current_session.store(0, Ordering::Relaxed);
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
    let owner = state.owner().await?;
    let ts = now();
    let (method, pw) = state.db.get_peer_encryption(&owner, &to).await;
    let payload = if method == "aes-gcm" {
        if let Some(pw) = pw {
            crypto::encrypt(&pw, &text)
        } else {
            text.clone()
        }
    } else {
        text.clone()
    };
    state.db.insert_message(&owner, &to, "out", ts, &text).await;

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
    let since = since.unwrap_or(0);
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
    let owner = state.owner().await?;
    state
        .db
        .set_peer_encryption(&owner, &peer, &method, password.as_deref())
        .await;
    Ok(())
}

#[tauri::command]
pub async fn get_peer_encryption(
    state: State<'_, Arc<AppState>>,
    peer: String,
) -> Result<serde_json::Value, String> {
    let owner = state.owner().await?;
    let (method, _) = state.db.get_peer_encryption(&owner, &peer).await;
    Ok(serde_json::json!({ "method": method }))
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, Arc<AppState>>,
    peer: String,
) -> Result<Vec<crate::db::Message>, String> {
    let owner = state.owner().await?;
    Ok(state.db.get_messages(&owner, &peer).await)
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    // Only wipe the currently logged-in account's local data. Any other
    // account that happens to share this SQLite file (e.g. a second app
    // instance) keeps its own rows untouched.
    if let Ok(owner) = state.owner().await {
        state.db.wipe_owner(&owner).await;
    }
    Ok(())
}