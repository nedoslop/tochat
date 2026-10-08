use std::sync::Arc;
use std::time::Duration;

use base64::{engine::general_purpose::STANDARD, Engine};
use serde::Serialize;
use serde_json::json;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;
use tokio::sync::oneshot;

use crate::crypto::{validate_secret, EncryptionConfig, EncryptionMethod};
use crate::db::LocalMsg;
use crate::protocol::{ClientMsg, MessageKind, UserStatus};
use crate::state::{read_theme, theme_path, user_db_path, AppState};
use crate::util::{now_ms, random_id};
use crate::ws;

pub const NOTES_PEER: i64 = 0;

// ---------- auth ----------

#[tauri::command]
pub async fn register(base_url: String, username: String, password: String) -> Result<(), String> {
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
    status: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let st = state.inner().clone();
    let s = UserStatus::parse(&status);
    ws::connect(st, base_url, username, password, s).await
}

#[tauri::command]
pub async fn disconnect(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let st = state.inner().clone();
    ws::disconnect(&st).await
}

/// Returns true iff the main window is visible AND focused.
#[tauri::command]
pub fn is_window_focused(app: tauri::AppHandle) -> bool {
    crate::ws::is_window_focused(&app)
}

// ---------- username resolution ----------

#[tauri::command]
pub async fn resolve_user(
    username: String,
    state: State<'_, Arc<AppState>>,
) -> Result<i64, String> {
    let db = state.active_db().await?;
    if let Some(id) = db.peer_id_by_username(&username).await {
        return Ok(id);
    }
    drop(db);

    let (tx, rx) = oneshot::channel();
    {
        let mut lookups = state.pending_lookups.lock().await;
        lookups.insert(username.clone(), tx);
    }

    let ws_tx = {
        let ws = state.ws.lock().await;
        ws.as_ref()
            .map(|s| s.tx.clone())
            .ok_or_else(|| "not connected".to_string())?
    };
    ws_tx
        .send(ClientMsg::GetProfile { username: username.clone() })
        .map_err(|_| "connection closed".to_string())?;

    match tokio::time::timeout(Duration::from_secs(6), rx).await {
        Ok(Ok(Some(id))) => Ok(id),
        Ok(Ok(None)) => Err(format!("user '{username}' does not exist")),
        Ok(Err(_)) => Err("lookup cancelled".into()),
        Err(_) => {
            state.pending_lookups.lock().await.remove(&username);
            Err("lookup timed out".into())
        }
    }
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
pub async fn send_message(
    to: i64,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<LocalMsg, String> {
    send_media(to, "text".into(), text, state).await
}

#[tauri::command]
pub async fn send_media(
    to: i64,
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
        db.upsert_message(NOTES_PEER, &id, "out", ts, ts, k.as_str(), &payload, true, true)
            .await;
        let wire = state.encrypt_for_wire(NOTES_PEER, &payload).await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf {
                id: id.clone(),
                ts,
                edit_ts: ts,
                kind: k,
                payload: wire,
            });
        }
        return Ok(LocalMsg {
            id,
            direction: "out".into(),
            ts,
            edit_ts: ts,
            kind: k.as_str().into(),
            payload,
            plaintext: true,
            read: true,
        });
    }

    let id = random_id();
    let ts = now_ms();
    send_outgoing(&state, to, &id, ts, ts, k, &payload, false).await?;
    Ok(LocalMsg {
        id,
        direction: "out".into(),
        ts,
        edit_ts: ts,
        kind: k.as_str().into(),
        payload,
        plaintext: true,
        read: false,
    })
}

#[tauri::command]
pub async fn edit_message(
    peer: i64,
    id: String,
    text: String,
    state: State<'_, Arc<AppState>>,
) -> Result<LocalMsg, String> {
    if text.is_empty() {
        return Err("empty message".into());
    }
    let db = state.active_db().await?;
    let (ts, kind, read) = db
        .get_messages(peer, None, None)
        .await
        .iter()
        .find(|m| m.id == id)
        .map(|m| (m.ts, m.kind.clone(), m.read))
        .unwrap_or_else(|| (now_ms(), "text".into(), false));
    let k = kind_from_str(&kind);
    let edit_ts = now_ms();

    if peer == NOTES_PEER {
        db.upsert_message(NOTES_PEER, &id, "out", ts, edit_ts, k.as_str(), &text, true, true)
            .await;
        let wire = state.encrypt_for_wire(NOTES_PEER, &text).await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf {
                id: id.clone(),
                ts,
                edit_ts,
                kind: k,
                payload: wire,
            });
        }
        return Ok(LocalMsg {
            id,
            direction: "out".into(),
            ts,
            edit_ts,
            kind: k.as_str().into(),
            payload: text,
            plaintext: true,
            read: true,
        });
    }

    send_outgoing(&state, peer, &id, ts, edit_ts, k, &text, true).await?;
    Ok(LocalMsg {
        id,
        direction: "out".into(),
        ts,
        edit_ts,
        kind: k.as_str().into(),
        payload: text,
        plaintext: true,
        read,
    })
}

#[tauri::command]
pub async fn delete_message(
    peer: i64,
    id: String,
    state: State<'_, Arc<AppState>>,
) -> Result<LocalMsg, String> {
    let db = state.active_db().await?;
    let (ts, kind, read) = db
        .get_messages(peer, None, None)
        .await
        .iter()
        .find(|m| m.id == id)
        .map(|m| (m.ts, m.kind.clone(), m.read))
        .unwrap_or_else(|| (now_ms(), "text".into(), false));
    let k = kind_from_str(&kind);
    let edit_ts = now_ms();

    if peer == NOTES_PEER {
        db.upsert_message(NOTES_PEER, &id, "out", ts, edit_ts, k.as_str(), "", true, true)
            .await;
        let wire = state.encrypt_for_wire(NOTES_PEER, "").await?;
        let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
        if let Some(tx) = tx {
            let _ = tx.send(ClientMsg::SendToSelf {
                id: id.clone(),
                ts,
                edit_ts,
                kind: k,
                payload: wire,
            });
        }
        return Ok(LocalMsg {
            id,
            direction: "out".into(),
            ts,
            edit_ts,
            kind: k.as_str().into(),
            payload: String::new(),
            plaintext: true,
            read: true,
        });
    }

    send_outgoing(&state, peer, &id, ts, edit_ts, k, "", true).await?;
    Ok(LocalMsg {
        id,
        direction: "out".into(),
        ts,
        edit_ts,
        kind: k.as_str().into(),
        payload: String::new(),
        plaintext: true,
        read,
    })
}

#[allow(clippy::too_many_arguments)]
async fn send_outgoing(
    state: &Arc<AppState>,
    to: i64,
    id: &str,
    ts: i64,
    edit_ts: i64,
    kind: MessageKind,
    plaintext: &str,
    is_edit: bool,
) -> Result<(), String> {
    let db = state.active_db().await?;
    let wire_payload = state.encrypt_for_wire(to, plaintext).await?;

    db.upsert_message(to, id, "out", ts, edit_ts, kind.as_str(), plaintext, true, false)
        .await;

    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref()
            .map(|s| s.tx.clone())
            .ok_or_else(|| "not connected".to_string())?
    };

    let msg = if is_edit {
        ClientMsg::Edit {
            to,
            id: id.to_string(),
            ts,
            edit_ts,
            kind,
            payload: wire_payload,
        }
    } else {
        ClientMsg::Send {
            to,
            id: id.to_string(),
            ts,
            kind,
            payload: wire_payload,
        }
    };

    tx.send(msg).map_err(|_| "connection closed".to_string())?;
    Ok(())
}

#[tauri::command]
pub async fn pull_history(
    from: i64,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if from == NOTES_PEER {
        return Ok(());
    }
    let tx = {
        let ws = state.ws.lock().await;
        // A history pull is background work scheduled by the sync loop.
        // If the socket isn't installed yet (transient during connect /
        // reconnect), just quietly skip — the caller will retry.
        match ws.as_ref() {
            Some(s) => s.tx.clone(),
            None => return Ok(()),
        }
    };
    // A closed channel is also a soft failure: the reader task will emit
    // `disconnected` on its own, and the sync loop will retry after the
    // next reconnect. Don't spam the JS console with it.
    let _ = tx.send(ClientMsg::PullHistory { from, since, limit, before });
    Ok(())
}

#[tauri::command]
pub async fn mark_read(
    peer: i64,
    up_to_ts: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    if peer == NOTES_PEER {
        return Ok(());
    }
    if let Ok(db) = state.active_db().await {
        db.mark_incoming_read(peer, up_to_ts).await;
    }
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::ReadReceipt { to: peer, up_to_ts });
    }
    Ok(())
}

#[tauri::command]
pub async fn get_unread_counts(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<(i64, i64)>, String> {
    let db = state.active_db().await?;
    Ok(db.unread_counts().await)
}

#[tauri::command]
pub async fn list_pending(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        match ws.as_ref() {
            Some(s) => s.tx.clone(),
            None => return Ok(()),
        }
    };
    let _ = tx.send(ClientMsg::ListPending);
    Ok(())
}

#[tauri::command]
pub async fn delete_account(
    password: String,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref()
            .map(|s| s.tx.clone())
            .ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::DeleteAccount { password })
        .map_err(|_| "connection closed".to_string())?;
    Ok(())
}

// ---------- profile ----------

#[tauri::command]
pub async fn set_profile(
    display_name: Option<String>,
    avatar: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let tx = {
        let ws = state.ws.lock().await;
        ws.as_ref()
            .map(|s| s.tx.clone())
            .ok_or_else(|| "not connected".to_string())?
    };
    tx.send(ClientMsg::SetProfile { display_name, avatar })
        .map_err(|_| "connection closed".to_string())?;
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
pub async fn clear_chat(peer: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(peer).await;
    Ok(())
}

#[tauri::command]
pub async fn leave_chat(peer: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(peer).await;
    drop(db);
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::LeaveChat { peer });
    }
    Ok(())
}

#[tauri::command]
pub async fn block_user(user_id: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let db = state.active_db().await?;
    db.clear_peer(user_id).await;
    drop(db);
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::BlockUser { user_id });
    }
    Ok(())
}

#[tauri::command]
pub async fn unblock_user(user_id: i64, state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::UnblockUser { user_id });
    }
    Ok(())
}

#[tauri::command]
pub async fn list_blocked(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::ListBlocked);
    }
    Ok(())
}

// ---------- local storage ----------

#[tauri::command]
pub async fn get_messages(
    peer: i64,
    before_ts: Option<i64>,
    limit: Option<u32>,
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<LocalMsg>, String> {
    let db = state.active_db().await?;
    Ok(db.get_messages(peer, before_ts, limit).await)
}

#[tauri::command]
pub async fn has_messages_before(
    peer: i64,
    before_ts: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let db = state.active_db().await?;
    Ok(db.has_messages_before(peer, before_ts).await)
}

#[tauri::command]
pub async fn get_peer(
    id: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<Option<crate::db::LocalPeer>, String> {
    let db = state.active_db().await?;
    Ok(db.get_peer(id).await)
}

#[tauri::command]
pub async fn list_local_peers(
    state: State<'_, Arc<AppState>>,
) -> Result<Vec<crate::db::LocalPeer>, String> {
    let db = state.active_db().await?;
    Ok(db.list_peers().await)
}

#[tauri::command]
pub async fn update_badge(count: i64, app: tauri::AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        let title = if count > 0 { format!("Chat ({count})") } else { "Chat".to_string() };
        let _ = window.set_title(&title);
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            if count > 0 {
                let _ = window.set_badge_count(Some(count));
            } else {
                let _ = window.set_badge_count(None);
            }
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
        if let Some(db) = guard {
            db.wipe_all().await;
            true
        } else {
            false
        }
    };
    let me = state.me.read().await.clone();
    let server_url = state.server_url.read().await.clone();
    if had_db {
        *state.db.write().await = None;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    if let (Some(m), Some(url)) = (me, server_url) {
        let path = user_db_path(&state.data_dir, &m.username, &url);
        let _ = std::fs::remove_file(&path);
    }
    {
        let mut map = state.encryption.write().await;
        map.clear();
    }
    Ok(())
}

// ---------- file saving ----------

/// Decodes `data:<mime>;base64,<payload>` into raw bytes.
fn decode_data_url(url: &str) -> Result<Vec<u8>, String> {
    let rest = url
        .strip_prefix("data:")
        .ok_or_else(|| "not a data url".to_string())?;
    let (meta, payload) = rest
        .split_once(',')
        .ok_or_else(|| "malformed data url".to_string())?;
    if !meta.ends_with(";base64") {
        return Err("only base64 data urls are supported".to_string());
    }
    STANDARD
        .decode(payload.trim())
        .map_err(|e| format!("base64 decode error: {e}"))
}

/// Opens a native "save file" dialog and writes the decoded data-URL bytes
/// to the chosen path. Silently no-ops if the user cancels the dialog.
#[tauri::command]
pub async fn save_data_url(
    data_url: String,
    suggested_name: Option<String>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let bytes = decode_data_url(&data_url)?;
    let name = suggested_name
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "download".to_string());

    let (tx, rx) = oneshot::channel();
    app.dialog()
        .file()
        .set_file_name(&name)
        .save_file(move |picked| {
            let _ = tx.send(picked);
        });

    let picked = rx.await.map_err(|_| "dialog cancelled".to_string())?;
    let Some(path) = picked else {
        // User pressed Cancel — not an error.
        return Ok(());
    };
    let path_buf = path.into_path().map_err(|e| e.to_string())?;
    std::fs::write(&path_buf, bytes).map_err(|e| format!("write failed: {e}"))?;
    Ok(())
}

// ---------- encryption ----------

#[derive(Serialize)]
pub struct EncryptionInfo {
    pub method: String,
    pub has_secret: bool,
}

#[tauri::command]
pub async fn get_encryption(
    peer: i64,
    state: State<'_, Arc<AppState>>,
) -> Result<EncryptionInfo, String> {
    let map = state.encryption.read().await;
    let cfg = map.get_config(peer);
    Ok(EncryptionInfo {
        method: cfg.method.as_str().to_string(),
        has_secret: cfg.secret.as_deref().map_or(false, |s| !s.is_empty()),
    })
}

#[tauri::command]
pub async fn set_encryption(
    peer: i64,
    method: String,
    secret: Option<String>,
    state: State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let m = EncryptionMethod::parse(&method);
    let mut secret = secret.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());

    if secret.is_none() {
        let map = state.encryption.read().await;
        let existing = map.get_config(peer);
        if existing.method == m {
            secret = existing.secret.clone();
        }
    }

    if m != EncryptionMethod::None {
        match secret.as_deref() {
            None => return Err("a secret is required for this method".into()),
            Some(s) if !validate_secret(m, s) => {
                return Err(match m {
                    EncryptionMethod::PreSharedKey => {
                        "pre-shared key must be 64 hex characters (32 bytes)".into()
                    }
                    _ => "invalid secret".into(),
                });
            }
            _ => {}
        }
    }

    let config = EncryptionConfig { method: m, secret: secret.clone() };

    let db = state.active_db().await?;
    db.set_encryption(peer, m.as_str(), secret.as_deref()).await;

    let mut map = state.encryption.write().await;
    map.set(peer, config);
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
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
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