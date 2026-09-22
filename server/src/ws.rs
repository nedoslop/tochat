use std::sync::atomic::{AtomicU64, Ordering};

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use dashmap::mapref::entry::Entry;
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{AppState, OnlineSession};
use crate::util::{hash_password, now};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

pub async fn ws_handler(
    ws: WebSocketUpgrade,
    headers: HeaderMap,
    State(state): State<AppState>,
) -> impl IntoResponse {
    let Some(auth) = headers.get("Authorization").and_then(|v| v.to_str().ok()) else {
        return reject("missing Authorization header");
    };
    let Some((username, password)) = auth.split_once(':') else {
        return reject("invalid Authorization header");
    };

    let last_seen = match state.db.get_user(username).await {
        Some((h, ls)) if h == hash_password(password) => ls,
        _ => return reject("invalid credentials"),
    };

    let username = username.to_string();
    ws.on_upgrade(move |socket| handle_socket(socket, state, username, last_seen))
}

fn reject(msg: &str) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": msg })),
    )
        .into_response()
}

async fn handle_socket(socket: WebSocket, state: AppState, username: String, last_seen: i64) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let session_id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Take over any existing session for this user.
    match state.online.entry(username.clone()) {
        Entry::Occupied(mut e) => {
            let old = e.get();
            let _ = old.tx.send(ServerMsg::Close);
            e.insert(OnlineSession {
                tx: tx.clone(),
                session_id,
            });
        }
        Entry::Vacant(v) => {
            v.insert(OnlineSession {
                tx: tx.clone(),
                session_id,
            });
        }
    }

    let _ = tx.send(ServerMsg::AuthOk {
        username: username.clone(),
        last_seen,
    });

    let peers = state.db.list_peers(&username).await;
    let pending = state.db.list_pending_chats(&username).await;
    let _ = tx.send(ServerMsg::Peers {
        peers: peers.clone(),
    });
    let _ = tx.send(ServerMsg::PendingChats { users: pending });

    // Send initial online status for peers that are already online.
    for p in &peers {
        if state.online.contains_key(p) {
            let _ = tx.send(ServerMsg::PeerOnline {
                username: p.clone(),
            });
        }
    }

    // Notify peers that we are online.
    for p in &peers {
        if let Some(peer) = state.online.get(p) {
            let _ = peer.tx.send(ServerMsg::PeerOnline {
                username: username.clone(),
            });
        }
    }

    let mut send_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let is_close = matches!(m, ServerMsg::Close);
            let s = match serde_json::to_string(&m) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if sender.send(Message::Text(s.into())).await.is_err() {
                break;
            }
            if is_close {
                let _ = sender.send(Message::Close(None)).await;
                break;
            }
        }
    });

    let state2 = state.clone();
    let username2 = username.clone();
    let tx2 = tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(m)) = receiver.next().await {
            let text = match m {
                Message::Text(t) => t.to_string(),
                Message::Close(_) => break,
                _ => continue,
            };
            let cm: ClientMsg = match serde_json::from_str(&text) {
                Ok(c) => c,
                Err(_) => continue,
            };
            handle_client(cm, &username2, &state2, &tx2).await;
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    // Only clean up if we are still the current session.
    let still_current = state
        .online
        .get(&username)
        .map(|e| e.session_id == session_id)
        .unwrap_or(false);

    if still_current {
        state.online.remove(&username);
        state.db.update_last_seen(&username, now()).await;

        for p in &peers {
            if let Some(peer) = state.online.get(p) {
                let _ = peer.tx.send(ServerMsg::PeerOffline {
                    username: username.clone(),
                });
            }
        }
    }
}

async fn handle_client(
    cm: ClientMsg,
    me: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match cm {
        ClientMsg::Send { to, payload } => handle_send(me, &to, payload, state, tx).await,
        ClientMsg::PullHistory { from, since } => {
            handle_pull_history(me, &from, since, state, tx).await
        }
        ClientMsg::HistoryResponse { to, messages } => {
            handle_history_response(me, &to, messages, state).await
        }
        ClientMsg::ListPending => {
            let users = state.db.list_pending_chats(me).await;
            let _ = tx.send(ServerMsg::PendingChats { users });
        }
        ClientMsg::DeleteAccount { password } => {
            handle_delete_account(me, &password, state, tx).await
        }
    }
}

async fn handle_send(
    me: &str,
    to: &str,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == to {
        let _ = tx.send(ServerMsg::Error {
            msg: "cannot send to yourself".into(),
        });
        return;
    }

    if !state.db.user_exists(to).await {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", to),
        });
        return;
    }

    let ts = now();

    match state.db.get_relationship(me, to).await {
        None => {
            // First message → register pending relationship. Sender stores
            // message locally; we do not deliver anything to `to`.
            state.db.ensure_relationship_initiated(me, to).await;
            // Notify receiver if online that they have a new pending chat.
            if let Some(peer) = state.online.get(to) {
                let pending = state.db.list_pending_chats(to).await;
                let _ = peer.tx.send(ServerMsg::PendingChats { users: pending });
            }
        }
        Some((initiator, established)) => {
            if established {
                if let Some(peer) = state.online.get(to) {
                    let _ = peer.tx.send(ServerMsg::Message {
                        from: me.to_string(),
                        payload,
                        ts,
                    });
                }
                // If peer offline → sender stores locally, peer will pull later.
            } else if initiator == me {
                // We initiated, other side hasn't pulled yet → store locally only.
            } else {
                // We are the receiver of a pending init → we must pull first.
                let _ = tx.send(ServerMsg::Error {
                    msg: format!("pull history from {} first", to),
                });
            }
        }
    }
}

async fn handle_pull_history(
    me: &str,
    from: &str,
    since: i64,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == from {
        let _ = tx.send(ServerMsg::Error {
            msg: "cannot pull history from yourself".into(),
        });
        return;
    }
    if !state.db.user_exists(from).await {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", from),
        });
        return;
    }

    let Some((initiator, established)) = state.db.get_relationship(me, from).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("no chat with {}", from),
        });
        return;
    };

    // While pending, only the initiator's history may be pulled.
    if !established && initiator != from {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("{} has not messaged you", from),
        });
        return;
    }

    match state.online.get(from) {
        Some(peer) => {
            let _ = peer.tx.send(ServerMsg::PullHistoryRequest {
                from: me.to_string(),
                since,
            });
        }
        None => {
            let _ = tx.send(ServerMsg::Error {
                msg: format!("peer {} is offline, try again later", from),
            });
        }
    }
}

async fn handle_history_response(me: &str, to: &str, messages: Vec<StoredMsg>, state: &AppState) {
    if me == to {
        return;
    }

    // Any first exchange of history establishes the chat; notify both sides
    // so each immediately pulls from the other (bidirectional sync).
    if let Some((_initiator, established)) = state.db.get_relationship(me, to).await {
        if !established {
            state.db.establish_relationship(me, to).await;
            // Notify both sides that they are now peers.
            if let Some(peer) = state.online.get(to) {
                let _ = peer.tx.send(ServerMsg::PeerOnline {
                    username: me.to_string(),
                });
                let pending = state.db.list_pending_chats(to).await;
                let _ = peer.tx.send(ServerMsg::PendingChats { users: pending });
            }
            if let Some(my_tx) = state.online.get(me) {
                let _ = my_tx.tx.send(ServerMsg::PeerOnline {
                    username: to.to_string(),
                });
                let pending = state.db.list_pending_chats(me).await;
                let _ = my_tx.tx.send(ServerMsg::PendingChats { users: pending });
            }
        }
    }

    if let Some(peer) = state.online.get(to) {
        let _ = peer.tx.send(ServerMsg::HistoryResponse {
            from: me.to_string(),
            messages,
        });
    }
}

async fn handle_delete_account(
    me: &str,
    password: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some((h, _)) = state.db.get_user(me).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: "unknown user".into(),
        });
        return;
    };
    if h != hash_password(password) {
        let _ = tx.send(ServerMsg::Error {
            msg: "invalid password".into(),
        });
        return;
    }

    let peers = state.db.list_peers(me).await;
    for p in &peers {
        if let Some(peer_tx) = state.online.get(p) {
            let _ = peer_tx.tx.send(ServerMsg::PeerOffline {
                username: me.to_string(),
            });
        }
    }

    state.db.delete_user(me).await;
    state.online.remove(me);

    let _ = tx.send(ServerMsg::Close);
}