use std::sync::Arc;

use serde::Serialize;
use serde_json::json;
use tauri::{Manager, State};

use crate::crypto::{validate_secret, EncryptionConfig, EncryptionMethod};
use crate::db::LocalMsg;
use crate::protocol::{ClientMsg, MessageKind, UserStatus};
use crate::state::{encryption_path, read_theme, theme_path, user_db_path, AppState};
use crate::util::{now_ms, random_id};
use crate::ws;

pub const NOTES_PEER: &str = "__notes__";

// ---------- auth ----------

#[tauri::command]
pub async fn register(base_url: String, username: String, password: String) -> Result<(), String> {
    let url = format!("{}/register", base_url.trim().trim_end_matches('/'));
    let client = reqwest::Client::new();
    let resp = client.post(&url)
        .json(&json!({ "username": username, "password": password }))
        .send().await.map_err(|e| format!("network error: {e}"))?;
    if resp.status().is_success() { Ok(()) } else {
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Err(format!("register failed ({status}): {text}"))
    }
}

#[tauri::command]
pub async fn connect(base_url: String, username: String, password: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let st = state.inner().clone();
    ws::connect(st, base_url, username, password).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let st = state.inner().clone();
    ws::disconnect(&st).await
}

// ---------- messaging ----------

fn kind_from_str(s: &str) -> MessageKind {
    match s {
        "image" => MessageKind::Image,
        "audio" => MessageKind::Audio,
        "file" => MessageKind::File,
        _ => MessageKind::Text,
    }
}

#[tauri::command]
pub async fn send_message(to: String, text: String, state: State<'_, Arc<AppState>>) -> Result<LocalMsg, String> {
    send_media(to, "text".into(), text, state).await
}

#[tauri::command]
pub async fn send_media(
    to: String,
    kind: String,
    payload: String,
    state: State<'_, Arc<AppState>>,
) -> Result<LocalMsg, String> {
    if payload.is_empty() {
        return Err("empty message".into());
    }
    let k = kind_from_str(&kind);

    if to == NOTES_PEER {
        let db = state.active_db().await?;
        let id = random_id();
        let ts = now_ms();
        db.upsert_message(&to, &id, "out", ts, ts, k.as_str(), &payload, true, true).await;
        let wire = state.encrypt_for_wire(&payload).await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf { id: id.clone(), ts, edit_ts: ts, kind: k, payload: wire });
        }
        return Ok(LocalMsg {
            id, direction: "out".into(), ts, edit_ts: ts,
            kind: k.as_str().into(), payload, plaintext: true, read: true,
        });
    }

    let id = random_id();
    let ts = now_ms();
    send_outgoing(&state, &to, &id, ts, ts, k, &payload, false).await?;
    Ok(LocalMsg {
        id, direction: "out".into(), ts, edit_ts: ts,
        kind: k.as_str().into(), payload, plaintext: true, read: false,
    })
}

#[tauri::command]
pub async fn edit_message(peer: String, id: String, text: String, state: State<'_, Arc<AppState>>) -> Result<LocalMsg, String> {
    if text.is_empty() { return Err("empty message".into()); }
    let db = state.active_db().await?;
    let (ts, kind, read) = db.get_messages(&peer, None, None).await.iter()
        .find(|m| m.id == id)
        .map(|m| (m.ts, m.kind.clone(), m.read))
        .unwrap_or_else(|| (now_ms(), "text".into(), false));
    let k = kind_from_str(&kind);
    let edit_ts = now_ms();

    if peer == NOTES_PEER {
        db.upsert_message(&peer, &id, "out", ts, edit_ts, k.as_str(), &text, true, true).await;
        let wire = state.encrypt_for_wire(&text).await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf { id: id.clone(), ts, edit_ts, kind: k, payload: wire });
        }
        return Ok(LocalMsg {
            id, direction: "out".into(), ts, edit_ts,
            kind: k.as_str().into(), payload: text, plaintext: true, read: true,
        });
    }

    send_outgoing(&state, &peer, &id, ts, edit_ts, k, &text, true).await?;
    Ok(LocalMsg {
        id, direction: "out".into(), ts, edit_ts,
        kind: k.as_str().into(), payload: text, plaintext: true, read,
    })
}

#[tauri::command]
pub async fn delete_message(peer: String, id: String, state: State<'_, Arc<AppState>>) -> Result<LocalMsg, String> {
    let db = state.active_db().await?;
    let (ts, kind, read) = db.get_messages(&peer, None, None).await.iter()
        .find(|m| m.id == id)
        .map(|m| (m.ts, m.kind.clone(), m.read))
        .unwrap_or_else(|| (now_ms(), "text".into(), false));
    let k = kind_from_str(&kind);
    let edit_ts = now_ms();

    if peer == NOTES_PEER {
        db.upsert_message(&peer, &id, "out", ts, edit_ts, k.as_str(), "", true, true).await;
        let wire = state.encrypt_for_wire("").await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf { id: id.clone(), ts, edit_ts, kind: k, payload: wire });
        }
        return Ok(LocalMsg {
            id, direction: "out".into(), ts, edit_ts,
            kind: k.as_str().into(), payload: String::new(), plaintext: true, read: true,
        });
    }

    send_outgoing(&state, &peer, &id, ts, edit_ts, k, "", true).await?;
    Ok(LocalMsg {
        id, direction: "out".into(), ts, edit_ts,
        kind: k.as_str().into(), payload: String::new(), plaintext: true, read,
    })
}

#[allow(clippy::too_many_arguments)]
async fn send_outgoing(
    state: &Arc<AppState>,
    to: &str,
    id: &str,
    ts: i64,
    edit_ts: i64,
    kind: MessageKind,
    plaintext: &str,
    is_edit: bool,
) -> Result<(), String> {
    let db = state.active_db().await?;
    let wire_payload = state.encrypt_for_wire(plaintext).await?;

    db.upsert_message(to, id, "out", ts, edit_ts, kind.as_str(), plaintext, true, false).await;

    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };

    let msg = if is_edit {
        ClientMsg::Edit { to: to.to_string(), id: id.to_string(), ts, edit_ts, kind, payload: wire_payload }
    } else {
        ClientMsg::Send { to: to.to_string(), id: id.to_string(), ts, kind, payload: wire_payload }
    };

    tx.send(msg).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn pull_history(
    from: String,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if from == NOTES_PEER { return Ok(()); }
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::PullHistory { from, since, limit, before })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn mark_read(peer: String, up_to_ts: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if peer == NOTES_PEER { return Ok(()); }
    // Mark local incoming messages as read.
    if let Ok(db) = state.active_db().await {
        db.mark_incoming_read(&peer, up_to_ts).await;
    }
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::ReadReceipt { to: peer, up_to_ts });
    }
    Ok(())
}

#[tauri::command]
pub async fn get_unread_counts(state: State<'_, Arc<AppState>>) -> Result<Vec<(String, i64)>, String> {
    let db = state.active_db().await?;
    Ok(db.unread_counts().await)
}

#[tauri::command]
pub async fn list_pending(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::ListPending).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn delete_account(password: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::DeleteAccount { password }).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

// ---------- profile ----------

#[tauri::command]
pub async fn get_profile(username: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::GetProfile { username }).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn set_profile(
    display_name: Option<String>,
    avatar: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref().map(|s| s.tx.clone()).ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::SetProfile { display_name, avatar }).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

// ---------- status / block / leave / clear ----------

#[tauri::command]
pub async fn set_status(status: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let s = UserStatus::parse(&status);
    *state.my_status.write().await = s;
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::SetStatus { status: s });
    }
    Ok(())
}

#[tauri::command]
pub async fn clear_chat(peer: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(&peer).await;
    Ok(())
}

#[tauri::command]
pub async fn leave_chat(peer: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(&peer).await;
    drop(db);
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx { let _ = tx.send(ClientMsg::LeaveChat { peer }); }
    Ok(())
}

#[tauri::command]
pub async fn block_user(username: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(&username).await;
    drop(db);
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx { let _ = tx.send(ClientMsg::BlockUser { username }); }
    Ok(())
}

#[tauri::command]
pub async fn unblock_user(username: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx { let _ = tx.send(ClientMsg::UnblockUser { username }); }
    Ok(())
}

#[tauri::command]
pub async fn list_blocked(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx { let _ = tx.send(ClientMsg::ListBlocked); }
    Ok(())
}

// ---------- local storage ----------

#[tauri::command]
pub async fn get_messages(
    peer: String,
    before_ts: Option<i64>,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LocalMsg>, String> {
    let db = state.active_db().await?;
    Ok(db.get_messages(&peer, before_ts, limit).await)
}

#[tauri::command]
pub async fn has_messages_before(
    peer: String,
    before_ts: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let db = state.active_db().await?;
    Ok(db.has_messages_before(&peer, before_ts).await)
}

#[tauri::command]
pub async fn update_badge(count: i64, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let title = if count > 0 { format!("Chat ({count})") } else { "Chat".to_string() };
        let _ = window.set_title(&title);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            let _ = window.set_badge_count(Some(count));
        }
        #[cfg(target_os = "macos")]
        {
            let label = if count > 0 { Some(count.to_string()) } else { None };
            let _ = window.set_badge_label(label);
        }
    }
    Ok(())
}

#[tauri::command]
pub async fn wipe_local_data(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let had_db = {
        let guard = state.db.read().await.clone();
        if let Some(db) = guard { db.wipe_all().await; true } else { false }
    };
    let me = state.me.read().await.clone();
    let server_url = state.server_url.read().await.clone();
    if had_db {
        *state.db.write().await = None;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if let (Some(name), Some(url)) = (me, server_url) {
        let path = user_db_path(&state.data_dir, &name, &url);
        let _ = std::fs::remove_file(&path);
    }
    let config = EncryptionConfig::default();
    { let mut enc = state.encryption.write().await; enc.set(config); }
    let _ = std::fs::remove_file(encryption_path(&state.data_dir));
    Ok(())
}

// ---------- encryption ----------

#[derive(Serialize)]
pub struct EncryptionInfo {
    pub method: String,
    pub has_secret: bool,
}

#[tauri::command]
pub async fn get_encryption(state: State<'_, Arc<AppState>>) -> Result<EncryptionInfo, String> {
    let enc = state.encryption.read().await;
    Ok(EncryptionInfo {
        method: enc.config.method.as_str().to_string(),
        has_secret: enc.config.secret.as_deref().map_or(false, |s| !s.is_empty()),
    })
}

#[tauri::command]
pub async fn set_encryption(
    method: String,
    secret: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let m = EncryptionMethod::parse(&method);
    let mut secret = secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    if secret.is_none() {
        let enc = state.encryption.read().await;
        if enc.config.method == m { secret = enc.config.secret.clone(); }
    }

    if m != EncryptionMethod::None {
        match secret.as_deref() {
            None => return Err("a secret is required for this method".into()),
            Some(s) if !validate_secret(m, s) => {
                return Err(match m {
                    EncryptionMethod::PreSharedKey => "pre-shared key must be 64 hex characters (32 bytes)".into(),
                    _ => "invalid secret".into(),
                });
            }
            _ => {}
        }
    }

    let config = EncryptionConfig { method: m, secret: secret.clone() };
    let json = serde_json::to_string(&config).map_err(|e| e.to_string())?;
    std::fs::write(encryption_path(&state.data_dir), json).map_err(|e| format!("persist error: {e}"))?;
    let mut enc = state.encryption.write().await;
    enc.set(config);
    Ok(())
}

#[tauri::command]
pub fn generate_psk() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

// ---------- theme ----------

#[tauri::command]
pub async fn get_theme(state: State<'_, Arc<AppState>>) -> Result<String, String> {
    Ok(read_theme(&state.data_dir))
}

#[tauri::command]
pub async fn set_theme(theme: String, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if !matches!(theme.as_str(), "system" | "light" | "dark") {
        return Err("invalid theme".into());
    }
    let json = serde_json::to_string(&json!({ "theme": theme })).map_err(|e| e.to_string())?;
    std::fs::write(theme_path(&state.data_dir), json).map_err(|e| format!("persist error: {e}"))?;
    apply_window_theme(&state.app, &theme);
    Ok(())
}

pub fn apply_window_theme(app: &tauri::AppHandle, theme: &str) {
    let Some(window) = app.get_webview_window("main") else { return; };
    let t = match theme {
        "light" => Some(tauri::Theme::Light),
        "dark" => Some(tauri::Theme::Dark),
        _ => None,
    };
    let _ = window.set_theme(t);
}

#[tauri::command]
pub fn is_release() -> bool {
    cfg!(not(debug_assertions))
}