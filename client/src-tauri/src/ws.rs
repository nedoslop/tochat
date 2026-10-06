use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tauri::{Emitter, Manager};
use tauri_plugin_notification::NotificationExt;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::db::Database;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{user_db_path, AppState, WsSession};

pub const NOTES_PEER: &str = "__notes__";

pub async fn connect(
    state: Arc<AppState>,
    base_url: String,
    username: String,
    password: String,
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
    let session_id = state
        .ws_counter
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    *state.db.write().await = Some(Arc::new(db));
    *state.me.write().await = Some(username.clone());
    *state.server_url.write().await = Some(base_url.clone());

    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let s = match serde_json::to_string(&m) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if sink.send(Message::Text(s.into())).await.is_err() {
                break;
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
                Message::Close(_) => break,
                _ => {}
            }
        }

        {
            let mut ws = reader_state.ws.lock().await;
            if ws.as_ref().map(|s| s.id == session_id).unwrap_or(false) {
                *ws = None;
            }
        }
        let _ = reader_state
            .app
            .emit("disconnected", json!(reader_username));
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
    Ok(())
}

async fn shutdown_current_session(state: &Arc<AppState>) {
    let old = state.ws.lock().await.take();
    let Some(s) = old else { return };

    drop(s.tx);
    let _ = tokio::time::timeout(Duration::from_millis(500), s.writer).await;
    s.reader.abort();
    let _ = s.reader.await;
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
            username,
            last_seen,
        } => {
            let _ = state.app.emit(
                "auth-ok",
                json!({ "username": username, "last_seen": last_seen }),
            );
        }
        ServerMsg::Peers { peers } => {
            let _ = state.app.emit("peers", json!(peers));
        }
        ServerMsg::PendingChats { users } => {
            let _ = state.app.emit("pending-chats", json!(users));
        }
        ServerMsg::PeerOnline { username } => {
            let _ = state.app.emit("peer-online", json!(username));
        }
        ServerMsg::PeerOffline { username } => {
            let _ = state.app.emit("peer-offline", json!(username));
        }
        ServerMsg::StatusUpdate { username, status } => {
            let _ = state.app.emit(
                "status-update",
                json!({ "username": username, "status": status.as_str() }),
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
            handle_note_incoming(state, id, ts, edit_ts, kind.as_str().to_string(), payload)
                .await;
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
                db.mark_read_up_to(&from, up_to_ts).await;
            }
            let _ = state.app.emit(
                "read-receipt",
                json!({ "peer": from, "up_to_ts": up_to_ts }),
            );
        }
        ServerMsg::Error { msg } => {
            let _ = state.app.emit("error", json!(msg));
        }
        ServerMsg::Close { reason } => {
            let _ = state.app.emit("session-closed", json!(reason));
        }
        ServerMsg::ChatLeft { peer } => {
            let _ = state.app.emit("chat-left", json!(peer));
        }
        ServerMsg::Blocked { users } => {
            let _ = state.app.emit("blocked", json!(users));
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_incoming(
    state: &Arc<AppState>,
    id: String,
    from: String,
    ts: i64,
    edit_ts: i64,
    kind: String,
    payload: String,
) {
    let (stored, is_plaintext) = state.decode_from_wire(&payload).await;

    if let Ok(db) = state.active_db().await {
        db.upsert_message(
            &from,
            &id,
            "in",
            ts,
            edit_ts,
            &kind,
            &stored,
            is_plaintext,
            true,
        )
        .await;
    }

    let focused = state
        .app
        .get_webview_window("main")
        .and_then(|w| w.is_focused().ok())
        .unwrap_or(false);
    if !focused {
        let preview = if stored.is_empty() {
            "(empty)".to_string()
        } else if stored.chars().count() > 60 {
            let mut s: String = stored.chars().take(57).collect();
            s.push_str("...");
            s
        } else {
            stored.clone()
        };
        let _ = state
            .app
            .notification()
            .builder()
            .title(format!("New message from {from}"))
            .body(preview)
            .show();
    }

    let _ = state.app.emit(
        "message",
        json!({
            "id": id,
            "peer": from,
            "direction": "in",
            "ts": ts,
            "edit_ts": edit_ts,
            "kind": kind,
            "payload": stored,
        }),
    );
}

async fn handle_note_incoming(
    state: &Arc<AppState>,
    id: String,
    ts: i64,
    edit_ts: i64,
    kind: String,
    payload: String,
) {
    let (stored, is_plaintext) = state.decode_from_wire(&payload).await;
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
            "peer": NOTES_PEER,
            "direction": "out",
            "ts": ts,
            "edit_ts": edit_ts,
            "kind": kind,
            "payload": stored,
        }),
    );
}

async fn handle_pull_request(
    state: &Arc<AppState>,
    from: String,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
) {
    let Ok(db) = state.active_db().await else {
        return;
    };
    let msgs = db.get_messages(&from).await;
    let me = state.me.read().await.clone().unwrap_or_default();

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
            } else if let Some(c) = enc.cipher.as_ref() {
                c.encrypt(&m.payload).unwrap_or_else(|| m.payload.clone())
            } else {
                m.payload.clone()
            };

            let (from_user, to_user) = if m.direction == "out" {
                (me.clone(), from.clone())
            } else {
                (from.clone(), me.clone())
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

async fn handle_history_response(state: &Arc<AppState>, from: String, messages: Vec<StoredMsg>) {
    let me = state.me.read().await.clone().unwrap_or_default();

    if let Ok(db) = state.active_db().await {
        for m in messages {
            let direction = if m.from == me { "out" } else { "in" };
            let (stored, is_plaintext) = state.decode_from_wire(&m.payload).await;

            db.upsert_message(
                &from,
                &m.id,
                direction,
                m.ts,
                m.edit_ts,
                m.kind.as_str(),
                &stored,
                is_plaintext,
                direction == "in",
            )
            .await;
        }
    }

    let _ = state.app.emit("history-received", json!(from));
}