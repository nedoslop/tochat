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
use futures_util::{sink::SinkExt, stream::StreamExt};
use serde_json::json;
use tokio::sync::mpsc;

use crate::protocol::{ClientMsg, MessageKind, ServerMsg, StoredMsg, UserStatus};
use crate::state::{AppState, OnlineSession};
use crate::util::{hash_password, now_ms};

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
    let Some((user_id, hash, last_seen)) = state.db.get_user(username).await else {
        return reject("invalid credentials");
    };
    if hash != hash_password(password) {
        return reject("invalid credentials");
    }
    let username = username.to_string();
    ws.on_upgrade(move |socket| handle_socket(socket, state, user_id, username, last_seen))
}

fn reject(msg: &str) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": msg })),
    )
        .into_response()
}

async fn handle_socket(
    socket: WebSocket,
    state: AppState,
    user_id: i64,
    username: String,
    last_seen: i64,
) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();
    let session_id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);

    let was_empty = state.add_session(
        user_id,
        OnlineSession {
            tx: tx.clone(),
            session_id,
            status: UserStatus::Online,
        },
    );

    let _ = tx.send(ServerMsg::AuthOk {
        username: username.clone(),
        last_seen,
    });
    let peers = state.db.list_peers(user_id).await;
    let pending = state.db.list_pending_chats(user_id).await;
    let blocked = state.db.list_blocks(user_id).await;
    let _ = tx.send(ServerMsg::Peers {
        peers: peers.clone(),
    });
    let _ = tx.send(ServerMsg::PendingChats { users: pending });
    let _ = tx.send(ServerMsg::Blocked { users: blocked });

    // Announce to peers only on the first session.
    if was_empty {
        for p in &peers {
            let Some(peer_id) = state.db.user_id(p).await else {
                continue;
            };
            state.send_to_user(
                peer_id,
                ServerMsg::PeerOnline {
                    username: username.clone(),
                },
            );
        }
    }

    // Tell the newly-connected client which of its peers are online right now.
    for p in &peers {
        let Some(peer_id) = state.db.user_id(p).await else {
            continue;
        };
        if state.is_online(peer_id) {
            let _ = tx.send(ServerMsg::PeerOnline {
                username: p.clone(),
            });
        }
    }

    let mut send_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let is_close = matches!(m, ServerMsg::Close { .. });
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
            handle_client(cm, user_id, session_id, &username2, &state2, &tx2).await;
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    let now_empty = state.remove_session(user_id, session_id);
    if now_empty {
        state.db.update_last_seen(user_id, now_ms()).await;
        let peers = state.db.list_peers(user_id).await;
        for p in &peers {
            let Some(peer_id) = state.db.user_id(p).await else {
                continue;
            };
            state.send_to_user(
                peer_id,
                ServerMsg::PeerOffline {
                    username: username.clone(),
                },
            );
        }
    }
}

async fn handle_client(
    cm: ClientMsg,
    me_id: i64,
    session_id: u64,
    me: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match cm {
        ClientMsg::Send {
            to,
            id,
            ts,
            kind,
            payload,
        } => handle_send(me_id, me, &to, id, ts, kind, payload, state, tx).await,
        ClientMsg::Edit {
            to,
            id,
            ts,
            edit_ts,
            kind,
            payload,
        } => handle_edit(me_id, me, &to, id, ts, edit_ts, kind, payload, state, tx).await,
        ClientMsg::SendToSelf {
            id,
            ts,
            edit_ts,
            kind,
            payload,
        } => {
            // Relay to all OTHER sessions of the same user (notes sync).
            state.send_to_user_except(
                me_id,
                session_id,
                ServerMsg::NoteMessage {
                    id,
                    ts,
                    edit_ts,
                    kind,
                    payload,
                },
            );
        }
        ClientMsg::PullHistory {
            from,
            since,
            limit,
            before,
        } => {
            handle_pull_history(me_id, me, &from, since, limit, before, state, tx).await
        }
        ClientMsg::HistoryResponse { to, messages } => {
            handle_history_response(me_id, me, &to, messages, state).await
        }
        ClientMsg::ListPending => {
            let users = state.db.list_pending_chats(me_id).await;
            let _ = tx.send(ServerMsg::PendingChats { users });
        }
        ClientMsg::DeleteAccount { password } => {
            handle_delete_account(me_id, me, &password, state, tx).await
        }
        ClientMsg::SetStatus { status } => {
            if let Some(mut sessions) = state.online.get_mut(&me_id) {
                for s in sessions.iter_mut() {
                    if s.session_id == session_id {
                        s.status = status;
                    }
                }
            }
            broadcast_status(state, me_id, me, status).await;
        }
        ClientMsg::LeaveChat { peer } => handle_leave_chat(me_id, me, &peer, state, tx).await,
        ClientMsg::BlockUser { username } => {
            handle_block(me_id, me, &username, state, tx).await
        }
        ClientMsg::UnblockUser { username } => {
            handle_unblock(me_id, me, &username, state, tx).await
        }
        ClientMsg::ListBlocked => {
            let users = state.db.list_blocks(me_id).await;
            let _ = tx.send(ServerMsg::Blocked { users });
        }
        ClientMsg::ReadReceipt { to, up_to_ts } => {
            let Some(to_id) = state.db.user_id(&to).await else {
                return;
            };
            if state.db.is_blocked(to_id, me_id).await {
                return;
            }
            state.send_to_user(
                to_id,
                ServerMsg::ReadReceipt {
                    from: me.to_string(),
                    up_to_ts,
                },
            );
        }
    }
}

async fn broadcast_status(state: &AppState, me_id: i64, me: &str, status: UserStatus) {
    let peers = state.db.list_peers(me_id).await;
    for p in &peers {
        let Some(pid) = state.db.user_id(p).await else {
            continue;
        };
        state.send_to_user(
            pid,
            ServerMsg::StatusUpdate {
                username: me.to_string(),
                status,
            },
        );
    }
}

/// Broadcast a PeerOnline announcement to both sides of a peer pair.
async fn notify_peer_pair(state: &AppState, me_id: i64, me: &str, to_id: i64, to: &str) {
    if state.is_online(to_id) {
        state.send_to_user(
            to_id,
            ServerMsg::PeerOnline {
                username: me.to_string(),
            },
        );
        if state.is_online(me_id) {
            state.send_to_user(
                me_id,
                ServerMsg::PeerOnline {
                    username: to.to_string(),
                },
            );
        }
    }
}

async fn handle_send(
    me_id: i64,
    me: &str,
    to: &str,
    id: String,
    ts: i64,
    kind: MessageKind,
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
    if payload.is_empty() {
        let _ = tx.send(ServerMsg::Error {
            msg: "empty message".into(),
        });
        return;
    }
    let Some(to_id) = state.db.user_id(to).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", to),
        });
        return;
    };

    if state.db.is_blocked(to_id, me_id).await {
        return;
    }

    let deliverable = match state.db.get_relationship(me_id, to_id).await {
        None => {
            state.db.ensure_relationship_initiated(me_id, to_id).await;
            false
        }
        Some((initiator_id, established)) => {
            let implicit_accept = !established && initiator_id != me_id;
            if implicit_accept {
                state.db.establish_relationship(me_id, to_id).await;
                notify_peer_pair(state, me_id, me, to_id, to).await;
            }
            established || implicit_accept
        }
    };

    if deliverable {
        state.send_to_user(
            to_id,
            ServerMsg::Message {
                id,
                from: me.to_string(),
                ts,
                edit_ts: ts,
                kind,
                payload,
            },
        );
    }
}

async fn handle_edit(
    me_id: i64,
    me: &str,
    to: &str,
    id: String,
    ts: i64,
    edit_ts: i64,
    kind: MessageKind,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == to {
        return;
    }
    let Some(to_id) = state.db.user_id(to).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", to),
        });
        return;
    };
    if state.db.is_blocked(to_id, me_id).await {
        return;
    }
    let established = matches!(
        state.db.get_relationship(me_id, to_id).await,
        Some((_, true))
    );
    if !established {
        return;
    }
    state.send_to_user(
        to_id,
        ServerMsg::Message {
            id,
            from: me.to_string(),
            ts,
            edit_ts,
            kind,
            payload,
        },
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_pull_history(
    me_id: i64,
    me: &str,
    from: &str,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == from {
        let _ = tx.send(ServerMsg::Error {
            msg: "cannot pull from yourself".into(),
        });
        return;
    }
    let Some(from_id) = state.db.user_id(from).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", from),
        });
        return;
    };
    if state.db.is_blocked(from_id, me_id).await {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("{} has blocked you", from),
        });
        return;
    }
    let Some((initiator_id, established)) = state.db.get_relationship(me_id, from_id).await
    else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("no chat with {}", from),
        });
        return;
    };
    if !established && initiator_id != from_id {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("{} has not messaged you", from),
        });
        return;
    }
    if !state.is_online(from_id) {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("peer {} is offline, try again later", from),
        });
        return;
    }
    state.send_to_user(
        from_id,
        ServerMsg::PullHistoryRequest {
            from: me.to_string(),
            since,
            limit,
            before,
        },
    );
}

async fn handle_history_response(
    me_id: i64,
    me: &str,
    to: &str,
    messages: Vec<StoredMsg>,
    state: &AppState,
) {
    if me == to {
        return;
    }
    let Some(to_id) = state.db.user_id(to).await else {
        return;
    };
    if state.db.is_blocked(to_id, me_id).await {
        return;
    }
    if let Some((_, established)) = state.db.get_relationship(me_id, to_id).await {
        if !established {
            state.db.establish_relationship(me_id, to_id).await;
            notify_peer_pair(state, me_id, me, to_id, to).await;
        }
    }
    state.send_to_user(
        to_id,
        ServerMsg::HistoryResponse {
            from: me.to_string(),
            messages,
        },
    );
}

async fn handle_leave_chat(
    me_id: i64,
    me: &str,
    peer: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some(peer_id) = state.db.user_id(peer).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", peer),
        });
        return;
    };
    state.db.delete_relationship(me_id, peer_id).await;
    let _ = tx.send(ServerMsg::ChatLeft {
        peer: peer.to_string(),
    });
    state.send_to_user(
        peer_id,
        ServerMsg::ChatLeft {
            peer: me.to_string(),
        },
    );
}

async fn handle_block(
    me_id: i64,
    me: &str,
    username: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == username {
        return;
    }
    let Some(uid) = state.db.user_id(username).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user '{}' does not exist", username),
        });
        return;
    };
    state.db.add_block(me_id, uid).await;
    let users = state.db.list_blocks(me_id).await;
    let _ = tx.send(ServerMsg::Blocked { users });
    let _ = tx.send(ServerMsg::ChatLeft {
        peer: username.to_string(),
    });
    state.send_to_user(
        uid,
        ServerMsg::ChatLeft {
            peer: me.to_string(),
        },
    );
}

async fn handle_unblock(
    me_id: i64,
    _me: &str,
    username: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some(uid) = state.db.user_id(username).await else {
        return;
    };
    state.db.remove_block(me_id, uid).await;
    let users = state.db.list_blocks(me_id).await;
    let _ = tx.send(ServerMsg::Blocked { users });
}

async fn handle_delete_account(
    me_id: i64,
    me: &str,
    password: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some((id, h, _)) = state.db.get_user(me).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: "unknown user".into(),
        });
        return;
    };
    if id != me_id || h != hash_password(password) {
        let _ = tx.send(ServerMsg::Error {
            msg: "invalid password".into(),
        });
        return;
    }
    let peers = state.db.list_peers(me_id).await;
    let mut peer_ids = Vec::with_capacity(peers.len());
    for p in &peers {
        if let Some(pid) = state.db.user_id(p).await {
            peer_ids.push(pid);
        }
    }
    state.db.delete_user(me_id).await;
    state.online.remove(&me_id);
    for pid in peer_ids {
        state.send_to_user(
            pid,
            ServerMsg::PeerOffline {
                username: me.to_string(),
            },
        );
    }
    let _ = tx.send(ServerMsg::Close {
        reason: "account_deleted".into(),
    });
}