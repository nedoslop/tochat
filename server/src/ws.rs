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
use crate::state::AppState;
use crate::util::{hash_password, now};

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

    // --- single session per user (atomic) ---------------------------
    //
    // `DashMap::entry` locks the shard for the duration of this match,
    // so two concurrent upgrades for the same username can never both
    // observe a vacant slot. The second one gets `Occupied` and is
    // rejected; the already-connected session is left untouched.
    let already_connected = match state.online.entry(username.clone()) {
        Entry::Occupied(_) => true,
        Entry::Vacant(v) => {
            v.insert(tx.clone());
            false
        }
    };

    if already_connected {
        let msg = serde_json::to_string(&ServerMsg::Error {
            msg: "another session is already connected for this user".into(),
        })
        .unwrap();
        let _ = sender.send(Message::Text(msg.into())).await;
        let _ = sender.send(Message::Close(None)).await;
        // NOTE: do *not* fall through to the cleanup block below — we
        // must not remove the existing session's entry from `online`.
        return;
    }

    // Send the initial snapshot.
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

    // Notify established peers we came online.
    for p in &peers {
        if let Some(peer_tx) = state.online.get(p) {
            let _ = peer_tx.send(ServerMsg::PeerOnline {
                username: username.clone(),
            });
        }
    }

    // ---- sender task ------------------------------------------------
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

    // ---- receiver task ----------------------------------------------
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

    // ---- cleanup ----------------------------------------------------
    //
    // Only reached by the session that actually owns the slot. A rejected
    // duplicate returned above, so it can never erase a live session.
    state.online.remove(&username);
    state.db.update_last_seen(&username, now()).await;

    for p in &peers {
        if let Some(peer_tx) = state.online.get(p) {
            let _ = peer_tx.send(ServerMsg::PeerOffline {
                username: username.clone(),
            });
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

    let ts = now();

    match state.db.get_relationship(me, to).await {
        // No relationship yet -> the first message is a pending initiation.
        // We record it but do **not** deliver anything to `to`.
        None => {
            state.db.ensure_relationship_initiated(me, to).await;
            let _ = tx.send(ServerMsg::Error {
                msg: format!(
                    "chat with {} initiated; {} must pull history to open it",
                    to, to
                ),
            });
        }
        Some((initiator, established)) => {
            if established {
                if let Some(peer) = state.online.get(to) {
                    let _ = peer.send(ServerMsg::Message {
                        from: me.to_string(),
                        payload,
                        ts,
                    });
                }
                // Offline: message stays on the client's disk, nothing to do.
            } else if initiator == me {
                // We initiated, the other side has not pulled yet.
                let _ = tx.send(ServerMsg::Error {
                    msg: format!("{} has not pulled your history yet", to),
                });
            } else {
                // We are on the receiving side of a pending initiation.
                // We must pull history first.
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
    let Some((initiator, established)) = state.db.get_relationship(me, from).await else {
        // No pending message and no established chat -> refuse.
        let _ = tx.send(ServerMsg::Error {
            msg: format!("{} has not messaged you", from),
        });
        return;
    };

    // Only the other side (the initiator) can be pulled from
    // until the chat has been established.
    if !established && initiator != from {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("{} has not messaged you", from),
        });
        return;
    }

    match state.online.get(from) {
        Some(peer) => {
            let _ = peer.send(ServerMsg::PullHistoryRequest {
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
    // Only the initiator of a not-yet-established relationship can establish it.
    if let Some((initiator, established)) = state.db.get_relationship(me, to).await {
        if initiator == me && !established {
            state.db.establish_relationship(me, to).await;

            // Both sides now know the other is a peer.
            if let Some(peer) = state.online.get(to) {
                let _ = peer.send(ServerMsg::PeerOnline {
                    username: me.to_string(),
                });
            }
            if let Some(my_tx) = state.online.get(me) {
                let _ = my_tx.send(ServerMsg::PeerOnline {
                    username: to.to_string(),
                });
            }
        }
    }

    if let Some(peer) = state.online.get(to) {
        let _ = peer.send(ServerMsg::HistoryResponse {
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
            let _ = peer_tx.send(ServerMsg::PeerOffline {
                username: me.to_string(),
            });
        }
    }

    state.db.delete_user(me).await;
    state.online.remove(me);

    let _ = tx.send(ServerMsg::Close);
}
