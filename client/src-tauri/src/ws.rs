use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::crypto;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{AppState, WsHandle};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub async fn connect(
    app: AppHandle,
    state: std::sync::Arc<AppState>,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let ws_url = base_url
        .replace("http://", "ws://")
        .replace("https://", "wss://");
    let url = format!("{}/login", ws_url.trim_end_matches('/'));

    let mut request = url.into_client_request().map_err(|e| e.to_string())?;
    let auth = format!("{}:{}", username, password);
    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&auth).map_err(|e| e.to_string())?,
    );

    let (ws_stream, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;
    let (mut write, mut read) = ws_stream.split();

    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let session_id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Mark ourselves as the current session FIRST so any stale reader/writer
    // from a previous connect in this same process will see it is no longer
    // current and stop emitting UI events / mutating state.
    state.current_session.store(session_id, Ordering::Relaxed);

    // Set the owner *before* the socket starts exchanging messages, so any
    // DB access triggered by incoming frames is already correctly scoped.
    {
        let mut u = state.username.lock().await;
        *u = Some(username.clone());
    }
    {
        let mut ws = state.ws.lock().await;
        *ws = Some(WsHandle {
            tx: tx.clone(),
            session_id,
        });
    }

    // Writer task: pumps outgoing ClientMsg into the socket.
    tokio::spawn(async move {
        while let Some(msg) = rx.recv().await {
            let text = match serde_json::to_string(&msg) {
                Ok(t) => t,
                Err(_) => continue,
            };
            if write.send(Message::Text(text.into())).await.is_err() {
                break;
            }
        }
    });

    // Reader task: pumps incoming ServerMsg into UI events + DB.
    let app_recv = app.clone();
    let state_recv = state.clone();
    let username_clone = username.clone();
    tokio::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            let text = match msg {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            let server_msg: ServerMsg = match serde_json::from_str(&text) {
                Ok(m) => m,
                Err(_) => continue,
            };

            // Guard: a stale reader (its session was taken over by a newer
            // login) must NOT emit events like "session-closed", nor clear
            // the live WsHandle, nor touch the DB. Just drop its input.
            if !state_recv.is_current_session(session_id) {
                if matches!(server_msg, ServerMsg::Close) {
                    break;
                }
                continue;
            }

            handle_server_msg(
                &app_recv,
                &state_recv,
                session_id,
                &username_clone,
                server_msg,
            )
            .await;
        }

        // Cleanup — only if we are still the current session.
        if state_recv.is_current_session(session_id) {
            let mut ws = state_recv.ws.lock().await;
            if let Some(handle) = ws.as_ref() {
                if handle.session_id == session_id {
                    *ws = None;
                }
            }
            drop(ws);
            let _ = app_recv.emit("disconnected", ());
        }
    });

    Ok(())
}

async fn handle_server_msg(
    app: &AppHandle,
    state: &AppState,
    session_id: u64,
    me: &str,
    msg: ServerMsg,
) {
    match msg {
        ServerMsg::AuthOk { username, .. } => {
            let mut u = state.username.lock().await;
            *u = Some(username.clone());
            let _ = app.emit("auth-ok", serde_json::json!({ "username": username }));
        }
        ServerMsg::Peers { peers } => {
            let _ = app.emit("peers", peers);
        }
        ServerMsg::PendingChats { users } => {
            let _ = app.emit("pending-chats", users);
        }
        ServerMsg::PeerOnline { username } => {
            let _ = app.emit("peer-online", username);
        }
        ServerMsg::PeerOffline { username } => {
            let _ = app.emit("peer-offline", username);
        }
        ServerMsg::Message { from, payload, ts } => {
            let (method, pw) = state.db.get_peer_encryption(me, &from).await;
            let text = if method == "aes-gcm" {
                if let Some(pw) = pw {
                    crypto::decrypt(&pw, &payload)
                        .unwrap_or_else(|| format!("[decrypt failed] {}", payload))
                } else {
                    payload
                }
            } else {
                payload
            };
            state.db.insert_message(me, &from, "in", ts, &text).await;
            let _ = app.emit(
                "message",
                serde_json::json!({
                    "peer": from,
                    "direction": "in",
                    "ts": ts,
                    "text": text,
                }),
            );
        }
        ServerMsg::PullHistoryRequest { from, since } => {
            let messages = state.db.get_messages(me, &from).await;
            let stored: Vec<StoredMsg> = messages
                .into_iter()
                .filter(|m| m.ts > since)
                .map(|m| {
                    let (from_field, to_field) = if m.direction == "out" {
                        (me.to_string(), from.clone())
                    } else {
                        (from.clone(), me.to_string())
                    };
                    StoredMsg {
                        from: from_field,
                        to: to_field,
                        ts: m.ts,
                        payload: m.text,
                    }
                })
                .collect();

            // Encrypt payloads if encryption is enabled for this peer.
            let (method, pw) = state.db.get_peer_encryption(me, &from).await;
            let stored: Vec<StoredMsg> = if method == "aes-gcm" {
                if let Some(pw) = pw {
                    stored
                        .into_iter()
                        .map(|mut m| {
                            m.payload = crypto::encrypt(&pw, &m.payload);
                            m
                        })
                        .collect()
                } else {
                    stored
                }
            } else {
                stored
            };

            let _ = send_msg(
                state,
                ClientMsg::HistoryResponse {
                    to: from,
                    messages: stored,
                },
            )
            .await;
        }
        ServerMsg::HistoryResponse { from, messages } => {
            for m in messages {
                let direction = if m.from == me { "out" } else { "in" };
                let (method, pw) = state.db.get_peer_encryption(me, &from).await;
                let text = if method == "aes-gcm" {
                    if let Some(pw) = pw {
                        crypto::decrypt(&pw, &m.payload)
                            .unwrap_or_else(|| format!("[decrypt failed] {}", m.payload))
                    } else {
                        m.payload
                    }
                } else {
                    m.payload
                };
                state
                    .db
                    .insert_message(me, &from, direction, m.ts, &text)
                    .await;
            }
            let _ = app.emit("history-received", from);
        }
        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }
        ServerMsg::Close => {
            // Only honour Close if we're the current session. A stale reader
            // receiving a takeover-Close from the server must not pop the
            // "session-closed" alert on the fresh page, and must not clear
            // the live WsHandle.
            if state.is_current_session(session_id) {
                let _ = app.emit("session-closed", ());
                let mut ws = state.ws.lock().await;
                if let Some(handle) = ws.as_ref() {
                    if handle.session_id == session_id {
                        *ws = None;
                    }
                }
            }
        }
    }
}

async fn send_msg(state: &AppState, msg: ClientMsg) -> Result<(), String> {
    let ws = state.ws.lock().await;
    if let Some(handle) = ws.as_ref() {
        handle.tx.send(msg).map_err(|e| e.to_string())
    } else {
        Err("not connected".into())
    }
}