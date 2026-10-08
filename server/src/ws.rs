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

use crate::protocol::{ClientMsg, MessageKind, PeerInfo, ServerMsg, StoredMsg};
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

async fn resolve_peers(state: &AppState, ids: &[i64]) -> Vec<PeerInfo> {
    let mut out = Vec::with_capacity(ids.len());
    for &id in ids {
        if let Some(username) = state.db.username(id).await {
            out.push(PeerInfo { id, username });
        }
    }
    out
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

    // Inherit the last known status for this user. This is what makes
    // "I was Busy, closed the app, reopened it" survive: the server still
    // remembers Busy, so peers see Busy immediately when we reconnect.
    let initial_status = state.status_of(user_id);

    let _was_empty = state.add_session(
        user_id,
        OnlineSession {
            tx: tx.clone(),
            session_id,
            status: initial_status,
        },
    );

    // --- Initial handshake -------------------------------------------------

    let _ = tx.send(ServerMsg::AuthOk {
        user_id,
        username: username.clone(),
        last_seen,
    });

    // Echo our own status back so the client can sync its UI (e.g. if the
    // user changed status on another device while we were offline).
    let _ = tx.send(ServerMsg::StatusUpdate {
        user_id,
        status: initial_status,
    });

    if let Some((dn, av)) = state.db.get_profile(user_id).await {
        let _ = tx.send(ServerMsg::Profile {
            user_id,
            username: username.clone(),
            display_name: dn,
            avatar: av,
        });
    }

    let peers = state.db.list_peers(user_id).await;
    let pending = state.db.list_pending_chats(user_id).await;
    let blocked = state.db.list_blocks(user_id).await;

    let _ = tx.send(ServerMsg::Peers {
        peers: resolve_peers(&state, &peers).await,
    });
    let _ = tx.send(ServerMsg::PendingChats {
        users: resolve_peers(&state, &pending).await,
    });
    let _ = tx.send(ServerMsg::Blocked {
        users: resolve_peers(&state, &blocked).await,
    });

    // Ship each known peer's profile so the sidebar is fully populated
    // before the client even renders.
    for p in peers.iter().chain(pending.iter()).chain(blocked.iter()) {
        if let (Some(name), Some((dn, av))) =
            (state.db.username(*p).await, state.db.get_profile(*p).await)
        {
            let _ = tx.send(ServerMsg::Profile {
                user_id: *p,
                username: name,
                display_name: dn,
                avatar: av,
            });
        }
    }

    // Announce presence to peers on EVERY new session, with the current
    // status attached so peers don't flash "online" for a Busy/Away user.
    for p in &peers {
        state.send_to_user(
            *p,
            ServerMsg::PeerOnline {
                user_id,
                status: initial_status,
            },
        );
    }

    // Tell the newly-connected client which of its peers are online now
    // (and with which status).
    for p in &peers {
        if state.is_online(*p) {
            let _ = tx.send(ServerMsg::PeerOnline {
                user_id: *p,
                status: state.status_of(*p),
            });
        }
    }

    // --- Outbound pump -----------------------------------------------------
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

    // --- Inbound pump ------------------------------------------------------
    let state2 = state.clone();
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
            handle_client(cm, user_id, session_id, &state2, &tx2).await;
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    // --- Teardown ----------------------------------------------------------
    let now_empty = state.remove_session(user_id, session_id);
    if now_empty {
        state.db.update_last_seen(user_id, now_ms()).await;
        let peers = state.db.list_peers(user_id).await;
        for p in &peers {
            state.send_to_user(*p, ServerMsg::PeerOffline { user_id });
        }
        // NOTE: we intentionally keep `user_status[user_id]` around so that
        // the next reconnect can restore the same status without the client
        // having to re-announce it.
    }
}

async fn handle_client(
    cm: ClientMsg,
    me_id: i64,
    session_id: u64,
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
        } => handle_send(me_id, to, id, ts, kind, payload, state, tx).await,

        ClientMsg::Edit {
            to,
            id,
            ts,
            edit_ts,
            kind,
            payload,
        } => handle_edit(me_id, to, id, ts, edit_ts, kind, payload, state, tx).await,

        ClientMsg::SendToSelf {
            id,
            ts,
            edit_ts,
            kind,
            payload,
        } => {
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
        } => handle_pull_history(me_id, from, since, limit, before, state, tx).await,

        ClientMsg::HistoryResponse { to, messages } => {
            handle_history_response(me_id, to, messages, state).await
        }

        ClientMsg::ListPending => {
            let ids = state.db.list_pending_chats(me_id).await;
            let _ = tx.send(ServerMsg::PendingChats {
                users: resolve_peers(state, &ids).await,
            });
        }

        ClientMsg::DeleteAccount { password } => {
            handle_delete_account(me_id, &password, state, tx).await
        }

        ClientMsg::SetStatus { status } => {
            // Authoritative per-user status; survives reconnects.
            state.user_status.insert(me_id, status);
            if let Some(mut sessions) = state.online.get_mut(&me_id) {
                for s in sessions.iter_mut() {
                    s.status = status;
                }
            }
            // Broadcast to peers.
            let peers = state.db.list_peers(me_id).await;
            for p in &peers {
                state.send_to_user(
                    *p,
                    ServerMsg::StatusUpdate {
                        user_id: me_id,
                        status,
                    },
                );
            }
            // Mirror to our own other sessions (multi-device).
            state.send_to_user_except(
                me_id,
                session_id,
                ServerMsg::StatusUpdate {
                    user_id: me_id,
                    status,
                },
            );
        }

        ClientMsg::LeaveChat { peer } => handle_leave_chat(me_id, peer, state, tx).await,

        ClientMsg::BlockUser { user_id } => handle_block(me_id, user_id, state, tx).await,

        ClientMsg::UnblockUser { user_id } => handle_unblock(me_id, user_id, state, tx).await,

        ClientMsg::ListBlocked => {
            let ids = state.db.list_blocks(me_id).await;
            let _ = tx.send(ServerMsg::Blocked {
                users: resolve_peers(state, &ids).await,
            });
        }

        ClientMsg::ReadReceipt { to, up_to_ts } => {
            if state.db.is_blocked(to, me_id).await {
                return;
            }
            state.send_to_user(
                to,
                ServerMsg::ReadReceipt {
                    from: me_id,
                    up_to_ts,
                },
            );
        }

        ClientMsg::GetProfile { username } => {
            if let Some(uid) = state.db.user_id(&username).await {
                if let Some((dn, av)) = state.db.get_profile(uid).await {
                    let _ = tx.send(ServerMsg::Profile {
                        user_id: uid,
                        username,
                        display_name: dn,
                        avatar: av,
                    });
                }
            } else {
                let _ = tx.send(ServerMsg::Error {
                    msg: format!("user '{}' does not exist", username),
                });
            }
        }

        ClientMsg::SetProfile {
            display_name,
            avatar,
        } => {
            state
                .db
                .set_profile(me_id, display_name.as_deref(), avatar.as_deref())
                .await;

            // Echo to self so the client's own profile stays authoritative.
            if let Some(name) = state.db.username(me_id).await {
                let _ = tx.send(ServerMsg::Profile {
                    user_id: me_id,
                    username: name.clone(),
                    display_name: display_name.clone(),
                    avatar: avatar.clone(),
                });

                // Broadcast to established peers.
                let peers = state.db.list_peers(me_id).await;
                for p in peers {
                    state.send_to_user(
                        p,
                        ServerMsg::Profile {
                            user_id: me_id,
                            username: name.clone(),
                            display_name: display_name.clone(),
                            avatar: avatar.clone(),
                        },
                    );
                }
            }
        }
    }
}

/// Broadcast PeerOnline to both sides of a peer pair.
///
/// IMPORTANT: the "are they online?" check is on the SUBJECT of each
/// notification, not on the recipient. Otherwise, when Alice sends to
/// offline Bob, Alice would get told "Bob is online" (because Alice is
/// online), which is a lie that makes the client try to pull history and
/// get a "peer offline" error.
async fn notify_peer_pair(state: &AppState, me_id: i64, to_id: i64) {
    if state.is_online(me_id) {
        state.send_to_user(
            to_id,
            ServerMsg::PeerOnline {
                user_id: me_id,
                status: state.status_of(me_id),
            },
        );
    }
    if state.is_online(to_id) {
        state.send_to_user(
            me_id,
            ServerMsg::PeerOnline {
                user_id: to_id,
                status: state.status_of(to_id),
            },
        );
    }
}

async fn handle_send(
    me_id: i64,
    to_id: i64,
    id: String,
    ts: i64,
    kind: MessageKind,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me_id == to_id {
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
    if state.db.username(to_id).await.is_none() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", to_id),
        });
        return;
    }

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
                notify_peer_pair(state, me_id, to_id).await;
            }
            established || implicit_accept
        }
    };

    if deliverable {
        state.send_to_user(
            to_id,
            ServerMsg::Message {
                id,
                from: me_id,
                ts,
                edit_ts: ts,
                kind,
                payload,
            },
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_edit(
    me_id: i64,
    to_id: i64,
    id: String,
    ts: i64,
    edit_ts: i64,
    kind: MessageKind,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me_id == to_id {
        return;
    }
    if state.db.username(to_id).await.is_none() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", to_id),
        });
        return;
    }
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
            from: me_id,
            ts,
            edit_ts,
            kind,
            payload,
        },
    );
}

async fn handle_pull_history(
    me_id: i64,
    from_id: i64,
    since: i64,
    limit: Option<u32>,
    before: Option<i64>,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me_id == from_id {
        let _ = tx.send(ServerMsg::Error {
            msg: "cannot pull from yourself".into(),
        });
        return;
    }
    if state.db.username(from_id).await.is_none() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", from_id),
        });
        return;
    }
    if state.db.is_blocked(from_id, me_id).await {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} has blocked you", from_id),
        });
        return;
    }
    let Some((initiator_id, established)) = state.db.get_relationship(me_id, from_id).await else {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("no chat with user {}", from_id),
        });
        return;
    };
    if !established && initiator_id != from_id {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} has not messaged you", from_id),
        });
        return;
    }
    if !state.is_online(from_id) {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("peer {} is offline, try again later", from_id),
        });
        return;
    }
    state.send_to_user(
        from_id,
        ServerMsg::PullHistoryRequest {
            from: me_id,
            since,
            limit,
            before,
        },
    );
}

async fn handle_history_response(
    me_id: i64,
    to_id: i64,
    messages: Vec<StoredMsg>,
    state: &AppState,
) {
    if me_id == to_id {
        return;
    }
    if state.db.username(to_id).await.is_none() {
        return;
    }
    if state.db.is_blocked(to_id, me_id).await {
        return;
    }
    if let Some((_, established)) = state.db.get_relationship(me_id, to_id).await {
        if !established {
            state.db.establish_relationship(me_id, to_id).await;
            notify_peer_pair(state, me_id, to_id).await;
        }
    }
    state.send_to_user(
        to_id,
        ServerMsg::HistoryResponse {
            from: me_id,
            messages,
        },
    );
}

async fn handle_leave_chat(
    me_id: i64,
    peer_id: i64,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if state.db.username(peer_id).await.is_none() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", peer_id),
        });
        return;
    }
    state.db.delete_relationship(me_id, peer_id).await;
    let _ = tx.send(ServerMsg::ChatLeft { peer: peer_id });
    state.send_to_user(peer_id, ServerMsg::ChatLeft { peer: me_id });
}

async fn handle_block(
    me_id: i64,
    target_id: i64,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me_id == target_id {
        return;
    }
    if state.db.username(target_id).await.is_none() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", target_id),
        });
        return;
    }
    state.db.add_block(me_id, target_id).await;
    let ids = state.db.list_blocks(me_id).await;
    let _ = tx.send(ServerMsg::Blocked {
        users: resolve_peers(state, &ids).await,
    });
    let _ = tx.send(ServerMsg::ChatLeft { peer: target_id });
    state.send_to_user(target_id, ServerMsg::ChatLeft { peer: me_id });
}

async fn handle_unblock(
    me_id: i64,
    target_id: i64,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if state.db.username(target_id).await.is_none() {
        return;
    }
    state.db.remove_block(me_id, target_id).await;
    let ids = state.db.list_blocks(me_id).await;
    let _ = tx.send(ServerMsg::Blocked {
        users: resolve_peers(state, &ids).await,
    });
}

async fn handle_delete_account(
    me_id: i64,
    password: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some((_name, h, _)) = state.db.get_user_by_id(me_id).await else {
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
    let peers = state.db.list_peers(me_id).await;
    state.db.delete_user(me_id).await;
    state.online.remove(&me_id);
    state.user_status.remove(&me_id);
    for p in peers {
        state.send_to_user(p, ServerMsg::PeerOffline { user_id: me_id });
    }
    let _ = tx.send(ServerMsg::Close {
        reason: "account_deleted".into(),
    });
}

