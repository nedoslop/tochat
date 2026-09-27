use std::sync::atomic::Ordering;
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tauri::Emitter;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::AppState;

/// Opens a new WebSocket session, replacing any previous one.
pub async fn connect(
    state: Arc<AppState>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    // Bump the generation first so any stale reader recognises itself as old.
    let my_gen = state.ws_gen.fetch_add(1, Ordering::SeqCst) + 1;

    // Drop any existing outbound channel; the old writer will wind down.
    {
        let mut ws = state.ws.lock().await;
        *ws = None;
    }

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

    {
        let mut ws = state.ws.lock().await;
        *ws = Some(tx.clone());
    }
    *state.me.write().await = Some(username.clone());

    // Writer: pumps outbound ClientMsg to the socket.
    tokio::spawn(async move {
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

    // Reader: parses ServerMsg and dispatches.
    let state_reader = state.clone();
    let username_reader = username.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = source.next().await {
            match msg {
                Message::Text(t) => {
                    if let Ok(sm) = serde_json::from_str::<ServerMsg>(&t) {
                        handle_server_msg(&state_reader, sm).await;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }

        // Only the most recent session is allowed to clear global state.
        if state_reader.ws_gen.load(Ordering::SeqCst) != my_gen {
            return;
        }
        {
            let mut ws = state_reader.ws.lock().await;
            *ws = None;
        }
        let mut me = state_reader.me.write().await;
        if me.as_deref() == Some(username_reader.as_str()) {
            *me = None;
            drop(me);
            let _ = state_reader.app.emit("disconnected", ());
        }
    });

    Ok(())
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
            handle_incoming(state, id, from, ts, edit_ts, kind, payload).await;
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
            let mut ws = state.ws.lock().await;
            *ws = None;
        }
    }
}

/// Decrypts (if possible) and stores an incoming message/edit, then emits it.
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

    state
        .db
        .upsert_message(&from, &id, "in", ts, edit_ts, &kind, &stored, is_plaintext)
        .await;

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
    let msgs = state.db.get_messages(&from).await;
    let me = state.me.read().await.clone().unwrap_or_default();

    let mut wire: Vec<StoredMsg> = Vec::with_capacity(msgs.len());
    {
        let enc = state.encryption.read().await;
        for m in msgs {
            if m.edit_ts <= since {
                continue;
            }

            // Rows already stored as opaque are forwarded as-is; plaintext
            // rows are re-encrypted for the wire if a cipher is active.
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
                kind: m.kind,
                payload,
            });
        }
    }

    let tx = { state.ws.lock().await.clone() };
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

    for m in messages {
        let direction = if m.from == me { "out" } else { "in" };
        let (stored, is_plaintext) = state.decode_from_wire(&m.payload).await;

        state
            .db
            .upsert_message(
                &from,
                &m.id,
                direction,
                m.ts,
                m.edit_ts,
                &m.kind,
                &stored,
                is_plaintext,
            )
            .await;
    }

    let _ = state.app.emit("history-received", json!(from));
}
