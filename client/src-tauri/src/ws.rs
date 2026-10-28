use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;

#[cfg(not(target_os = "android"))]
use tauri::UserAttentionType;

use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::db::Database;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg, UserStatus};
use crate::sound::play_notification_chirp;
use crate::state::{user_db_path, AppState, Me, WsSession};

pub const NOTES_PEER: i64 = 0;

pub async fn connect(
    state: Arc<AppState>,
    base_url: String,
    username: String,
    password: String,
    initial_status: UserStatus,
) -> Result<(), String> {
    shutdown_current_session(&state).await;

    let db_path = user_db_path(&state.data_dir, &username, &base_url);
    let db = Database::open(&db_path).map_err(|e| format!("open db: {e}"))?;

    let ws_url = to_ws_url(&base_url)?;
    let mut req = ws_url
        .into_client_request()
        .map_err(|e| format!("bad ws url: {e}"))?;
    req.headers_mut().insert(
        "Authorization",
        format!("{username}:{password}")
            .parse()
            .map_err(|e| format!("bad auth header: {e}"))?,
    );

    let (stream, _resp) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect failed: {e}"))?;

    let (mut sink, mut source) = stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    let session_id = state
        .ws_counter
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    *state.db.write().await = Some(Arc::new(db));
    *state.me.write().await = Some(Me {
        id: 0, // placeholder, replaced on AuthOk
        username: username.clone(),
    });
    *state.server_url.write().await = Some(base_url.clone());
    *state.my_status.write().await = initial_status;

    // Load per-chat encryption configs from this user's local DB.
    state.load_encryption_from_db().await.ok();

    let writer = tokio::spawn(async move {
        let mut ping_interval = tokio::time::interval(Duration::from_secs(20));
        ping_interval.tick().await;

        loop {
            tokio::select! {
                Some(payload) = pong_rx.recv() => {
                    if sink.send(Message::Pong(payload.into())).await.is_err() {
                        break;
                    }
                }
                msg = rx.recv() => {
                    match msg {
                        Some(m) => {
                            let s = match serde_json::to_string(&m) {
                                Ok(s) => s,
                                Err(_) => continue,
                            };
                            if sink.send(Message::Text(s.into())).await.is_err() {
                                break;
                            }
                        }
                        None => break,
                    }
                }
                _ = ping_interval.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
        let _ = sink.close().await;
    });

    let reader_state = state.clone();
    let reader_username = username.clone();
    let reader = tokio::spawn(async move {
        while let Some(msg) = source.next().await {
            let msg = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            match msg {
                Message::Text(t) => {
                    if let Ok(sm) = serde_json::from_str::<ServerMsg>(&t) {
                        let is_close = matches!(sm, ServerMsg::Close { .. });
                        handle_server_msg(&reader_state, sm).await;
                        if is_close {
                            break;
                        }
                    }
                }
                Message::Ping(payload) => {
                    let _ = pong_tx.send(payload.to_vec());
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        // IMPORTANT: only announce the disconnect if THIS session was the
        // currently-active one. When `connect` is called while an older
        // session is still winding down (normal re-login, or the brief
        // window during shutdown), that older reader exits here *after*
        // `state.ws` has already been replaced with the new session. If
        // we blindly emitted `disconnected`, the JS side would call
        // `resetToLogin()` and kick the user straight back to the login
        // screen — the "sign in, then 1 s later signed out" symptom.
        let was_current = {
            let mut ws = reader_state.ws.lock().await;
            if ws.as_ref().map(|s| s.id == session_id).unwrap_or(false) {
                *ws = None;
                true
            } else {
                false
            }
        };
        if was_current {
            let _ = reader_state
                .app
                .emit("disconnected", json!(reader_username));
        }
    });

    *state.ws.lock().await = Some(WsSession {
        id: session_id,
        tx,
        reader,
        writer,
    });
    Ok(())
}

pub async fn disconnect(state: &Arc<AppState>) -> Result<(), String> {
    shutdown_current_session(state).await;
    *state.db.write().await = None;
    *state.me.write().await = None;
    *state.server_url.write().await = None;
    {
        let mut map = state.encryption.write().await;
        map.clear();
    }
    Ok(())
}

async fn shutdown_current_session(state: &Arc<AppState>) {
    let old = state.ws.lock().await.take();
    let Some(mut s) = old else { return };
    drop(s.tx);
    s.reader.abort();
    if tokio::time::timeout(Duration::from_millis(500), &mut s.writer).await.is_err() {
        s.writer.abort();
    }
    let _ = s.reader.await;
    let _ = s.writer.await;
}

fn to_ws_url(base: &str) -> Result<String, String> {
    let trimmed = base.trim().trim_end_matches('/');
    if let Some(rest) = trimmed.strip_prefix("https://") {
        Ok(format!("wss://{rest}/login"))
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        Ok(format!("ws://{rest}/login"))
    } else if trimmed.starts_with("ws://") || trimmed.starts_with("wss://") {
        Ok(format!("{trimmed}/login"))
    } else {
        Err(format!("unsupported scheme in base url: {base}"))
    }
}

async fn handle_server_msg(state: &Arc<AppState>, sm: ServerMsg) {
    match sm {
        ServerMsg::AuthOk {
            user_id,
            username,
            last_seen,
        } => {
            *state.me.write().await = Some(Me {
                id: user_id,
                username: username.clone(),
            });
            let _ = state.app.emit(
                "auth-ok",
                json!({
                    "user_id": user_id,
                    "username": username,
                    "last_seen": last_seen,
                }),
            );
        }

        ServerMsg::Peers { peers } => {
            let _ = state.app.emit("peers", json!(peers));
        }

        ServerMsg::PendingChats { users } => {
            let _ = state.app.emit("pending-chats", json!(users));
        }

        ServerMsg::PeerOnline { user_id, status } => {
            let _ = state.app.emit(
                "peer-online",
                json!({ "user_id": user_id, "status": status.as_str() }),
            );
        }

        ServerMsg::PeerOffline { user_id } => {
            let _ = state
                .app
                .emit("peer-offline", json!({ "user_id": user_id }));
        }

        ServerMsg::StatusUpdate { user_id, status } => {
            let me = state.me.read().await.clone();
            if let Some(me) = me {
                if me.id == user_id {
                    *state.my_status.write().await = status;
                }
            }
            let _ = state.app.emit(
                "status-update",
                json!({ "user_id": user_id, "status": status.as_str() }),
            );
        }

        ServerMsg::Message {
            id,
            from,
            ts,
            edit_ts,
            kind,
            payload,
        } => {
            handle_incoming(
                state,
                id,
                from,
                ts,
                edit_ts,
                kind.as_str().to_string(),
                payload,
            )
            .await;
        }

        ServerMsg::NoteMessage {
            id,
            ts,
            edit_ts,
            kind,
            payload,
        } => {
            handle_note_incoming(state, id, ts, edit_ts, kind.as_str().to_string(), payload).await;
        }

        ServerMsg::PullHistoryRequest {
            from,
            since,
            limit,
            before,
        } => {
            handle_pull_request(state, from, since, limit, before).await;
        }

        ServerMsg::HistoryResponse { from, messages } => {
            handle_history_response(state, from, messages).await;
        }

        ServerMsg::ReadReceipt { from, up_to_ts } => {
            if let Ok(db) = state.active_db().await {
                db.mark_read_up_to(from, up_to_ts).await;
            }
            let _ = state.app.emit(
                "read-receipt",
                json!({ "peer": from, "up_to_ts": up_to_ts }),
            );
        }

        ServerMsg::Error { msg } => {
            if let Some(name) = msg
                .strip_prefix("user '")
                .and_then(|s| s.strip_suffix("' does not exist"))
            {
                let mut lookups = state.pending_lookups.lock().await;
                if let Some(tx) = lookups.remove(name) {
                    let _ = tx.send(None);
                }
            }
            let _ = state.app.emit("error", json!(msg));
        }

        ServerMsg::Close { reason } => {
            let _ = state.app.emit("session-closed", json!(reason));
        }

        ServerMsg::ChatLeft { peer } => {
            if let Ok(db) = state.active_db().await {
                db.delete_peer(peer).await;
            }
            let _ = state.app.emit("chat-left", json!({ "peer": peer }));
        }

        ServerMsg::Blocked { users } => {
            let _ = state.app.emit("blocked", json!(users));
        }

        ServerMsg::Profile {
            user_id,
            username,
            display_name,
            avatar,
        } => {
            if let Ok(db) = state.active_db().await {
                db.upsert_peer(
                    user_id,
                    &username,
                    display_name.as_deref(),
                    avatar.as_deref(),
                )
                .await;
            }
            {
                let mut lookups = state.pending_lookups.lock().await;
                if let Some(tx) = lookups.remove(&username) {
                    let _ = tx.send(Some(user_id));
                }
            }
            let _ = state.app.emit(
                "profile",
                json!({
                    "user_id": user_id,
                    "username": username,
                    "display_name": display_name,
                    "avatar": avatar,
                }),
            );
        }
    }
}

async fn should_alert(state: &Arc<AppState>) -> bool {
    !matches!(*state.my_status.read().await, UserStatus::Busy)
}

pub fn is_window_focused(app: &tauri::AppHandle) -> bool {
    let Some(window) = app.get_webview_window("main") else {
        return false;
    };
    if window.is_minimized().unwrap_or(false) {
        return false;
    }
    window.is_focused().unwrap_or(false)
}

fn request_attention(state: &Arc<AppState>) {
    #[cfg(not(target_os = "android"))]
    {
        let app = state.app.clone();
        tokio::spawn(async move {
            for _ in 0..30 {
                if app.get_webview_window("main").is_none() {
                    return;
                }
                if is_window_focused(&app) {
                    if let Some(w) = app.get_webview_window("main") {
                        let _ = w.request_user_attention(None);
                    }
                    return;
                }
                if let Some(w) = app.get_webview_window("main") {
                    let _ = w.request_user_attention(Some(UserAttentionType::Critical));
                }
                tokio::time::sleep(Duration::from_millis(2000)).await;
            }
            if let Some(w) = app.get_webview_window("main") {
                let _ = w.request_user_attention(None);
            }
        });
    }
}

async fn peer_display_label(state: &Arc<AppState>, peer_id: i64) -> String {
    if let Ok(db) = state.active_db().await {
        if let Some(p) = db.get_peer(peer_id).await {
            if let Some(name) = p.display_name.filter(|s| !s.trim().is_empty()) {
                return name;
            }
            if !p.username.is_empty() {
                return p.username;
            }
        }
    }
    format!("user #{peer_id}")
}

#[allow(clippy::too_many_arguments)]
async fn handle_incoming(
    state: &Arc<AppState>,
    id: String,
    from: i64,
    ts: i64,
    edit_ts: i64,
    kind: String,
    payload: String,
) {
    let (stored, is_plaintext) = state.decode_from_wire(from, &payload).await;

    if let Ok(db) = state.active_db().await {
        db.upsert_message(
            from,
            &id,
            "in",
            ts,
            edit_ts,
            &kind,
            &stored,
            is_plaintext,
            false,
        )
        .await;
    }

    let window_focused = is_window_focused(&state.app);

    let _ = state.app.emit(
        "message",
        json!({
            "id": id,
            "peer": from,
            "from": from,
            "direction": "in",
            "ts": ts,
            "edit_ts": edit_ts,
            "kind": kind,
            "payload": stored,
            "plaintext": is_plaintext,
            "read": false,
            "window_focused": window_focused,
            "windowFocused": window_focused,
        }),
    );

    if window_focused {
        return;
    }
    if !should_alert(state).await {
        return;
    }

    play_notification_chirp();

    let display = peer_display_label(state, from).await;
    let preview = preview_for(&kind, &stored);
    let title = format!("New message from {display}");

    // Always emit in-app-notification so webview can display in-app banner
    // if system notifications are disabled or muted by the OS.
    let _ = state.app.emit(
        "in-app-notification",
        json!({ "title": title, "body": preview, "peer": from }),
    );

    let _ = state
        .app
        .notification()
        .builder()
        .title(title)
        .body(preview)
        .show();

    request_attention(state);
}

fn preview_for(kind: &str, text: &str) -> String {
    match kind {
        "image" => "📷 Photo".to_string(),
        "audio" => "🎤 Audio".to_string(),
        "file" => {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(text) {
                if let Some(n) = v.get("name").and_then(|n| n.as_str()) {
                    return format!("📎 {n}");
                }
            }
            "📎 File".to_string()
        }
        _ => {
            if text.is_empty() {
                "(empty)".to_string()
            } else if text.chars().count() > 80 {
                let mut s: String = text.chars().take(77).collect();
                s.push_str("...");
                s
            } else {
                text.to_string()
            }
        }
    }
}

async fn handle_note_incoming(
    state: &Arc<AppState>,
    id: String,
    ts: i64,
    edit_ts: i64,
    kind: String,
    payload: String,
) {
    let (stored, is_plaintext) = state.decode_from_wire(NOTES_PEER, &payload).await;
    if let Ok(db) = state.active_db().await {
        db.upsert_message(
            NOTES_PEER,
            &id,
            "out",
            ts,
            edit_ts,
            &kind,
            &stored,
            is_plaintext,
            true,
        )
        .await;
    }
    let _ = state.app.emit(
        "note-message",
        json!({
            "id": id,
            "ts": ts,
            "edit_ts": edit_ts,
            "kind": kind,
            "payload": stored,
        }),
    );
}

async fn handle_pull_request(
    state: &Arc<AppState>,
    from: i64,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
) {
    let Ok(db) = state.active_db().await else {
        return;
    };
    let msgs = db.get_messages(from, None, None).await;
    let Some(me) = state.me.read().await.clone() else {
        return;
    };

    let mut filtered: Vec<_> = msgs
        .into_iter()
        .filter(|m| m.edit_ts > since)
        .filter(|m| before.map_or(true, |b| m.ts < b))
        .collect();

    if let Some(lim) = limit {
        let lim = lim as usize;
        if filtered.len() > lim {
            let drop_count = filtered.len() - lim;
            filtered.drain(0..drop_count);
        }
    }

    let mut wire: Vec<StoredMsg> = Vec::with_capacity(filtered.len());
    {
        let enc = state.encryption.read().await;
        for m in filtered {
            let payload = if !m.plaintext {
                m.payload.clone()
            } else if let Some(c) = enc.cipher(from) {
                c.encrypt(&m.payload).unwrap_or_else(|| m.payload.clone())
            } else {
                m.payload.clone()
            };

            let (from_user, to_user) = if m.direction == "out" {
                (me.id, from)
            } else {
                (from, me.id)
            };

            let kind = match m.kind.as_str() {
                "image" => crate::protocol::MessageKind::Image,
                "audio" => crate::protocol::MessageKind::Audio,
                "file" => crate::protocol::MessageKind::File,
                _ => crate::protocol::MessageKind::Text,
            };

            wire.push(StoredMsg {
                id: m.id,
                from: from_user,
                to: to_user,
                ts: m.ts,
                edit_ts: m.edit_ts,
                kind,
                payload,
            });
        }
    }

    let tx = { state.ws.lock().await.as_ref().map(|s| s.tx.clone()) };
    if let Some(tx) = tx {
        let _ = tx.send(ClientMsg::HistoryResponse {
            to: from,
            messages: wire,
        });
    }
}

async fn handle_history_response(state: &Arc<AppState>, from: i64, messages: Vec<StoredMsg>) {
    let Some(me) = state.me.read().await.clone() else {
        return;
    };

    if let Ok(db) = state.active_db().await {
        for m in messages {
            let direction = if m.from == me.id { "out" } else { "in" };
            let (stored, is_plaintext) = state.decode_from_wire(from, &m.payload).await;
            let read = direction == "out";
            db.upsert_message(
                from,
                &m.id,
                direction,
                m.ts,
                m.edit_ts,
                m.kind.as_str(),
                &stored,
                is_plaintext,
                read,
            )
            .await;
        }
    }

    let _ = state.app.emit("history-received", json!({ "from": from, "peer": from }));
}
