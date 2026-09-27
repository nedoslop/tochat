use std::sync::Arc;

use tauri::State;

use crate::db::Database;
use crate::http_auth;
use crate::protocol::{ClientMsg, StoredMsg, KIND_TEXT};
use crate::state::AppState;
use crate::util::{new_msg_id, now_ms};
use crate::ws;

/// Registers a new user via HTTP.
#[tauri::command]
pub async fn register(
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    http_auth::register(&base_url, &username, &password).await
}

/// Connects to the server: opens the per-user DB and starts the WebSocket.
#[tauri::command]
pub async fn connect(
    state: State<'_, Arc<AppState>>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let state = state.inner().clone();

    // Clear any previous session (idempotent).
    clear_session(&state).await;

    // Open the per-user database.
    let db = Database::open(&state.data_dir, &username).map_err(|e| e.to_string())?;
    *state.db.write().await = Some(Arc::new(db));
    *state.me.write().await = Some(username.clone());

    // Start the WebSocket.
    if let Err(e) = ws::spawn(state.clone(), &base_url, &username, &password).await {
        clear_session(&state).await;
        return Err(e);
    }
    Ok(())
}

/// Closes the WebSocket and clears session state.
#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    clear_session(state.inner()).await;
    Ok(())
}

/// Sends a new text message to a peer (stored locally + relayed).
#[tauri::command]
pub async fn send_message(
    state: State<'_, Arc<AppState>>,
    to: String,
    text: String,
) -> Result<(), String> {
    if text.is_empty() {
        return Err("empty message".into());
    }
    let me = state.me().await?;
    let db = state.db().await?;
    let id = new_msg_id();
    let ts = now_ms();
    let stored = StoredMsg {
        id: id.clone(),
        from: me.clone(),
        to: to.clone(),
        ts,
        edit_ts: ts,
        kind: KIND_TEXT.to_string(),
        payload: text.clone(),
    };
    db.upsert(&stored).await;
    state
        .send(ClientMsg::Send {
            to,
            id,
            ts,
            kind: KIND_TEXT.to_string(),
            payload: text,
        })
        .await
}

/// Edits an existing message (text = "" means delete).
#[tauri::command]
pub async fn edit_message(
    state: State<'_, Arc<AppState>>,
    peer: String,
    id: String,
    text: String,
) -> Result<(), String> {
    let me = state.me().await?;
    let db = state.db().await?;
    let existing = db.get(&id).await.ok_or("message not found")?;
    let edit_ts = now_ms();
    let stored = StoredMsg {
        id: id.clone(),
        from: me,
        to: peer.clone(),
        ts: existing.ts,
        edit_ts,
        kind: existing.kind.clone(),
        payload: text.clone(),
    };
    db.upsert(&stored).await;
    state
        .send(ClientMsg::Edit {
            to: peer,
            id,
            ts: existing.ts,
            edit_ts,
            kind: existing.kind,
            payload: text,
        })
        .await
}

/// Deletes a message (empty payload, updated edit_ts).
#[tauri::command]
pub async fn delete_message(
    state: State<'_, Arc<AppState>>,
    peer: String,
    id: String,
) -> Result<(), String> {
    edit_message(state, peer, id, String::new()).await
}

/// Requests history from a peer since a given edit_ts (exclusive).
#[tauri::command]
pub async fn pull_history(
    state: State<'_, Arc<AppState>>,
    from: String,
    since: i64,
) -> Result<(), String> {
    state.send(ClientMsg::PullHistory { from, since }).await
}

/// Requests the current pending chat list from the server.
#[tauri::command]
pub async fn list_pending(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    state.send(ClientMsg::ListPending).await
}

/// Deletes the account on the server (password required).
#[tauri::command]
pub async fn delete_account(
    state: State<'_, Arc<AppState>>,
    password: String,
) -> Result<(), String> {
    state.send(ClientMsg::DeleteAccount { password }).await
}

/// Returns all locally stored messages with a peer.
#[tauri::command]
pub async fn get_messages(
    state: State<'_, Arc<AppState>>,
    peer: String,
) -> Result<Vec<crate::db::LocalMsg>, String> {
    let db = state.db().await?;
    Ok(db.all_for_peer(&peer).await)
}

/// Wipes the current user's local database file.
#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let me = state.me.read().await.clone();
    // Drop the open handle before deleting files.
    *state.db.write().await = None;
    if let Some(username) = me {
        let base = state.data_dir.join(format!("{}.db", crate::util::encode_username(&username)));
        let _ = std::fs::remove_file(&base);
        let _ = std::fs::remove_file(base.with_extension("db-wal"));
        let _ = std::fs::remove_file(base.with_extension("db-shm"));
    }
    Ok(())
}

/// Clears session state: aborts the WebSocket task, drops the DB handle.
async fn clear_session(state: &Arc<AppState>) {
    {
        let mut guard = state.ws.lock().await;
        if let Some(handle) = guard.take() {
            // Abort the pump task (its future drop closes the socket).
            handle.task.abort();
            drop(handle.tx);
        }
    }
    *state.db.write().await = None;
    *state.me.write().await = None;
}