use tauri::{AppHandle, Manager, State};
use tokio::sync::{mpsc, oneshot};

use crate::db::LocalMsg;
use crate::protocol::ClientMsg;
use crate::state::AppState;
use crate::util::now;
use crate::ws;
use crate::{crypto, http_auth};

#[derive(serde::Serialize)]
pub struct EncInfo {
    pub method: String,
    pub has_password: bool,
}

#[tauri::command]
pub async fn register(base_url: String, username: String, password: String) -> Result<(), String> {
    http_auth::register(&base_url, &username, &password).await
}

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    state: State<'_, AppState>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    {
        let inner = state.inner.lock().await;
        if inner.username.is_some() {
            return Err("already connected".into());
        }
    }

    // Per-user DB file — several clients can run side-by-side.
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    let user_dir = dir.join("users").join(&username);
    std::fs::create_dir_all(&user_dir).map_err(|e| e.to_string())?;
    let db =
        crate::db::Database::open(&user_dir.join("local.db")).map_err(|e| format!("db: {e}"))?;

    let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (ready_tx, ready_rx) = oneshot::channel::<Result<(), String>>();
    let (shutdown_tx, shutdown_rx) = oneshot::channel::<()>();

    let app2 = app.clone();
    let db2 = db.clone();
    let uname = username.clone();
    tokio::spawn(ws::run(
        app2,
        base_url,
        uname,
        password,
        tx.clone(),
        rx,
        ready_tx,
        shutdown_rx,
        db2,
    ));

    ready_rx.await.map_err(|e| e.to_string())??;

    let mut inner = state.inner.lock().await;
    inner.username = Some(username);
    inner.ws_tx = Some(tx);
    inner.shutdown_tx = Some(shutdown_tx);
    inner.db = Some(db);
    Ok(())
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    let mut inner = state.inner.lock().await;
    if let Some(tx) = inner.shutdown_tx.take() {
        let _ = tx.send(());
    }
    inner.username = None;
    inner.ws_tx = None;
    inner.db = None;
    Ok(())
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    to: String,
    text: String,
) -> Result<(), String> {
    let (tx, db) = {
        let inner = state.inner.lock().await;
        let tx = inner.ws_tx.clone().ok_or("not connected")?;
        let db = inner.db.clone().ok_or("no db")?;
        (tx, db)
    };

    let (method, pw) = db.get_encryption(&to).await;
    let payload = crypto::encrypt(&method, pw.as_deref(), &text)?;

    let ts = now();
    db.insert_message(&to, "out", ts, &text).await;

    tx.send(ClientMsg::Send { to, payload })
        .map_err(|_| "ws closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn pull_history(state: State<'_, AppState>, from: String) -> Result<(), String> {
    let tx = {
        state
            .inner
            .lock()
            .await
            .ws_tx
            .clone()
            .ok_or("not connected")?
    };
    tx.send(ClientMsg::PullHistory { from, since: 0 })
        .map_err(|_| "ws closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_pending(state: State<'_, AppState>) -> Result<(), String> {
    let tx = {
        state
            .inner
            .lock()
            .await
            .ws_tx
            .clone()
            .ok_or("not connected")?
    };
    tx.send(ClientMsg::ListPending)
        .map_err(|_| "ws closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_account(state: State<'_, AppState>, password: String) -> Result<(), String> {
    let tx = {
        state
            .inner
            .lock()
            .await
            .ws_tx
            .clone()
            .ok_or("not connected")?
    };
    tx.send(ClientMsg::DeleteAccount { password })
        .map_err(|_| "ws closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_peer_encryption(
    state: State<'_, AppState>,
    peer: String,
    method: String,
    password: Option<String>,
) -> Result<(), String> {
    let db = { state.inner.lock().await.db.clone().ok_or("no db")? };

    if method != "none" && method != "aes-gcm" {
        return Err(format!("unsupported method: {method}"));
    }
    if method == "aes-gcm" && password.as_deref().map_or(true, |p| p.is_empty()) {
        return Err("password required for aes-gcm".into());
    }
    let pw = if method == "aes-gcm" {
        password.as_deref()
    } else {
        None
    };
    db.set_encryption(&peer, &method, pw).await;
    Ok(())
}

#[tauri::command]
pub async fn get_peer_encryption(
    state: State<'_, AppState>,
    peer: String,
) -> Result<EncInfo, String> {
    let db = { state.inner.lock().await.db.clone().ok_or("no db")? };
    let (method, pw) = db.get_encryption(&peer).await;
    Ok(EncInfo {
        method,
        has_password: pw.is_some(),
    })
}

#[tauri::command]
pub async fn get_messages(
    state: State<'_, AppState>,
    peer: String,
) -> Result<Vec<LocalMsg>, String> {
    let db = { state.inner.lock().await.db.clone().ok_or("no db")? };
    Ok(db.get_messages(&peer).await)
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, AppState>) -> Result<(), String> {
    let db = { state.inner.lock().await.db.clone() };
    if let Some(db) = db {
        db.wipe().await;
    }
    Ok(())
}
