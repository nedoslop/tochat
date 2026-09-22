use serde::Serialize;
use tauri::{AppHandle, Manager, State};

use crate::db::LocalDb;
use crate::http_auth;
use crate::protocol::ClientMsg;
use crate::state::{AppState, Connection, EncConfig};
use crate::util::now;
use crate::ws;

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
    // Drop any previous connection so a reload/login doesn't leave a
    // stale websocket on the server.
    {
        let mut guard = state.conn.lock().unwrap();
        *guard = None;
    }

    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&dir).ok();
    let db = LocalDb::open(&dir, &username).map_err(|e| e.to_string())?;

    // Preload encryption configs.
    {
        let mut map = state.enc.lock().unwrap();
        map.clear();
    }
    for (peer, method, password) in db.all_peer_enc().await {
        state
            .enc
            .lock()
            .unwrap()
            .insert(peer, EncConfig { method, password });
    }

    let tx = ws::spawn_connection(
        app.clone(),
        state.inner().clone(),
        db.clone(),
        base_url.clone(),
        username.clone(),
        password.clone(),
    )?;

    *state.conn.lock().unwrap() = Some(Connection {
        username: username.clone(),
        base_url,
        tx,
    });
    *state.db.lock().unwrap() = Some(db);

    Ok(())
}

#[tauri::command]
pub async fn disconnect(state: State<'_, AppState>) -> Result<(), String> {
    *state.conn.lock().unwrap() = None;
    Ok(())
}

#[tauri::command]
pub async fn send_message(
    state: State<'_, AppState>,
    to: String,
    text: String,
) -> Result<(), String> {
    let (tx, db, me) = {
        let conn = state.conn.lock().unwrap();
        let Some(c) = conn.as_ref() else {
            return Err("not connected".into());
        };
        let db = state.db.lock().unwrap().clone();
        (c.tx.clone(), db, c.username.clone())
    };
    if to == me {
        return Err("cannot send to yourself".into());
    }
    let payload = encrypt_for(&state, &to, &text);
    tx.send(ClientMsg::Send {
        to: to.clone(),
        payload,
    })
    .map_err(|_| "connection closed".to_string())?;
    if let Some(db) = db {
        db.insert_message(&to, "out", now(), &text).await;
    }
    Ok(())
}

#[tauri::command]
pub async fn pull_history(state: State<'_, AppState>, from: String) -> Result<(), String> {
    let tx = {
        let conn = state.conn.lock().unwrap();
        let Some(c) = conn.as_ref() else {
            return Err("not connected".into());
        };
        c.tx.clone()
    };
    tx.send(ClientMsg::PullHistory { from, since: 0 })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn list_pending(state: State<'_, AppState>) -> Result<(), String> {
    let tx = {
        let conn = state.conn.lock().unwrap();
        let Some(c) = conn.as_ref() else {
            return Err("not connected".into());
        };
        c.tx.clone()
    };
    tx.send(ClientMsg::ListPending)
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_account(state: State<'_, AppState>, password: String) -> Result<(), String> {
    let tx = {
        let conn = state.conn.lock().unwrap();
        let Some(c) = conn.as_ref() else {
            return Err("not connected".into());
        };
        c.tx.clone()
    };
    tx.send(ClientMsg::DeleteAccount { password })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[derive(Serialize)]
pub struct EncInfo {
    pub method: String,
}

#[tauri::command]
pub async fn set_peer_encryption(
    state: State<'_, AppState>,
    peer: String,
    method: String,
    password: Option<String>,
) -> Result<(), String> {
    let db = state.db.lock().unwrap().clone();
    if let Some(db) = db {
        db.set_peer_enc(&peer, &method, password.as_deref()).await;
    }
    state
        .enc
        .lock()
        .unwrap()
        .insert(peer, EncConfig { method, password });
    Ok(())
}

#[tauri::command]
pub async fn get_peer_encryption(
    state: State<'_, AppState>,
    peer: String,
) -> Result<EncInfo, String> {
    let method = {
        let map = state.enc.lock().unwrap();
        map.get(&peer).map(|c| c.method.clone())
    };
    if let Some(m) = method {
        return Ok(EncInfo { method: m });
    }
    let db = state.db.lock().unwrap().clone();
    if let Some(db) = db {
        let (m, _pw) = db.get_peer_enc(&peer).await;
        return Ok(EncInfo { method: m });
    }
    Ok(EncInfo {
        method: "none".into(),
    })
}

#[derive(Serialize)]
pub struct MsgRow {
    pub direction: String,
    pub ts: i64,
    pub text: String,
}

#[tauri::command]
pub async fn get_messages(state: State<'_, AppState>, peer: String) -> Result<Vec<MsgRow>, String> {
    let db = state.db.lock().unwrap().clone();
    let Some(db) = db else {
        return Ok(Vec::new());
    };
    let rows = db.get_messages(&peer).await;
    Ok(rows
        .into_iter()
        .map(|(direction, ts, text)| MsgRow {
            direction,
            ts,
            text,
        })
        .collect())
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, AppState>) -> Result<(), String> {
    let db = state.db.lock().unwrap().clone();
    if let Some(db) = db {
        db.wipe().await;
    }
    Ok(())
}

fn encrypt_for(state: &AppState, peer: &str, text: &str) -> String {
    let cfg = {
        let map = state.enc.lock().unwrap();
        map.get(peer).cloned()
    };
    if let Some(cfg) = cfg {
        if cfg.method == "aes-gcm" {
            if let Some(pw) = cfg.password {
                if let Ok(enc) = crate::crypto::encrypt(text.as_bytes(), &pw) {
                    return enc;
                }
            }
        }
    }
    text.to_string()
}
