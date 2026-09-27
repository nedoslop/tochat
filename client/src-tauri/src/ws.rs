use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tauri::Emitter;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::db::Database;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{user_db_path, AppState, WsSession};

/// Opens a new WebSocket session, replacing any previous one.
///
/// Steps:
///   1. Fully tear down any existing session (abort + await tasks).
///   2. Open the per-user local database.
///   3. Establish the WebSocket connection.
///   4. Publish state (db, me, ws) and spawn the reader/writer tasks.
pub async fn connect(
    state: Arc<AppState>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    // 1. Shut down any previous session *before* we touch anything else.
    shutdown_current_session(&state).await;

    // 2. Open the per-user DB.
    let db_path = user_db_path(&state.data_dir, &username);
    let db = Database::open(&db_path).map_err(|e| format!("open db: {e}"))?;

    // 3. Establish the WebSocket connection.
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

    // 4. Publish state and spawn tasks.
    let (mut sink, mut source) = stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let session_id = state
        .ws_counter
        .fetch_add(1, std::sync::atomic::Ordering::SeqCst);

    *state.db.write().await = Some(Arc::new(db));
    *state.me.write().await = Some(username.clone());

    // Writer: drain outbound messages to the socket.
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
        // Graceful close on the way out.
        let _ = sink.close().await;
    });

    // Reader: parse and dispatch inbound messages.
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

        // If this session is still the active one, clear it so no commands
        // can try to use the dead sender.
        {
            let mut ws = reader_state.ws.lock().await;
            if ws.as_ref().map(|s| s.id == session_id).unwrap_or(false) {
                *ws = None;
            }
        }
        let _ = reader_state.app.emit("disconnected", json!(reader_username));
    });

    *state.ws.lock().await = Some(WsSession {
        id: session_id,
        tx,
        reader,
        writer,
    });

    Ok(())
}

/// Explicitly tears the current session down. Used by the logout button.
pub async fn disconnect(state: &Arc<AppState>) -> Result<(), String> {
    shutdown_current_session(state).await;
    *state.db.write().await = None;
    *state.me.write().await = None;
    Ok(())
}

/// Takes the active session (if any) and guarantees its tasks are stopped.
async fn shutdown_current_session(state: &Arc<AppState>) {
    let old = state.ws.lock().await.take();
    let Some(s) = old else { return };

    // Drop the sender so the writer's recv loop finishes on its own and can
    // send a proper WebSocket close frame.
    drop(s.tx);

    // Give the writer a brief window to close gracefully, then move on.
    let _ = tokio::time::timeout(Duration::from_millis(500), s.writer).await;

    // Abort the reader, then wait for it to actually stop. After this returns,
    // no old task can touch state.
    s.reader.abort();
    let _ = s.reader.await;
}

/// Converts "http(s)://host:port" to a ws(s) URL for `/login`.
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
        ServerMsg::Message {
            id,
            from,
            ts,
            edit_ts,
            kind,
            payload,
        } => {
            handle_incoming(state, id, from, ts, edit_ts, kind.as_str().to_string(), payload)
                .await;
        }
        ServerMsg::PullHistoryRequest { from, since } => {
            handle_pull_request(state, from, since).await;
        }
        ServerMsg::HistoryResponse { from, messages } => {
            handle_history_response(state, from, messages).await;
        }
        ServerMsg::Error { msg } => {
            let _ = state.app.emit("error", json!(msg));
        }
        ServerMsg::Close { reason } => {
            let _ = state.app.emit("session-closed", json!(reason));
            // The reader loop will notice `is_close` and break; its cleanup
            // path handles the rest.
        }
    }
}

/// Decrypts (if possible) and stores an incoming message/edit, then emits it.
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

    // If we no longer have a local DB (logout race), just drop silently.
    if let Ok(db) = state.active_db().await {
        db.upsert_message(&from, &id, "in", ts, edit_ts, &kind, &stored, is_plaintext)
            .await;
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

/// Serves a peer's pull request from our local store.
async fn handle_pull_request(state: &Arc<AppState>, from: String, since: i64) {
    let Ok(db) = state.active_db().await else {
        return;
    };
    let msgs = db.get_messages(&from).await;
    let me = state.me.read().await.clone().unwrap_or_default();

    let mut wire: Vec<StoredMsg> = Vec::with_capacity(msgs.len());
    {
        let enc = state.encryption.read().await;
        for m in msgs {
            if m.edit_ts <= since {
                continue;
            }

            // Opaque rows are forwarded as-is; plaintext rows are re-encrypted
            // for the wire if a cipher is active.
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

            wire.push(StoredMsg {
                id: m.id,
                from: from_user,
                to: to_user,
                ts: m.ts,
                edit_ts: m.edit_ts,
                kind: crate::protocol::MessageKind::Text,
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

/// Handles an incoming history response, storing the messages and notifying
/// the UI.
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
            )
            .await;
        }
    }

    let _ = state.app.emit("history-received", json!(from));
}