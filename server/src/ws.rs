use crate::protocol::{ClientMsg, HistoryResponseMsg, MessageKind, PeerInfo, ServerMsg};
use crate::state::{AppState, OnlineSession};
use crate::util::{hash_password, now_ms};
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
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::sync::mpsc;

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// WebSocket message/frame ceiling.
///
/// A 16 MiB attachment becomes ~21 MiB as a base64 data URL and ~28 MiB
/// after encryption (base64 again). History replies are split into bounded
/// parts client-side; this ceiling just needs to fit the largest single
/// message with headroom.
const MAX_WS_MESSAGE_BYTES: usize = 128 * 1024 * 1024;

/// Cheap type peek so `history_response` frames can be routed to the raw
/// parser without deserializing the (potentially huge) message bodies via
/// the tagged `ClientMsg` enum. Unknown fields are skipped without
/// allocating, so this pass is fast even for multi-MiB frames.
#[derive(Deserialize)]
struct TypePeek {
    #[serde(rename = "type")]
    kind: String,
}

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
    ws.max_message_size(MAX_WS_MESSAGE_BYTES)
        .max_frame_size(MAX_WS_MESSAGE_BYTES)
        .on_upgrade(move |socket| handle_socket(socket, state, user_id, username, last_seen))
}

fn reject(msg: &str) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": msg })),
    )
        .into_response()
}

async fn resolve_peers(state: &AppState, ids: &[i64]) -> Vec<PeerInfo> {
    state
        .db
        .peer_infos(ids)
        .await
        .into_iter()
        .map(|(id, username)| PeerInfo { id, username })
        .collect()
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
    // axum 0.8 wraps ping/pong payloads in `Bytes` (text in `Utf8Bytes`).
    // Carry `Bytes` end-to-end so pong replies need no copy.
    let (pong_tx, mut pong_rx) = mpsc::unbounded_channel::<axum::body::Bytes>();
    let session_id = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);

    // Inherit the last known status for this user ("was Busy, reopened app").
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

    // Ship each known peer's profile (ONE batched DB pass, not N queries).
    let mut profile_ids: Vec<i64> = peers
        .iter()
        .chain(pending.iter())
        .chain(blocked.iter())
        .copied()
        .collect();
    profile_ids.sort_unstable();
    profile_ids.dedup();
    for (id, name, dn, av) in state.db.profiles(&profile_ids).await {
        let _ = tx.send(ServerMsg::Profile {
            user_id: id,
            username: name,
            display_name: dn,
            avatar: av,
        });
    }

    // Announce presence to all related contacts on EVERY new session.
    let all_contacts = state.db.list_all_relationships(user_id).await;
    for p in &all_contacts {
        state.send_to_user(
            *p,
            ServerMsg::PeerOnline {
                user_id,
                status: initial_status,
            },
        );
    }

    // Tell the newly-connected client which of its peers are online now.
    for p in &all_contacts {
        if state.is_online(*p) {
            let _ = tx.send(ServerMsg::PeerOnline {
                user_id: *p,
                status: state.status_of(*p),
            });
        }
    }

    // --- Outbound pump -----------------------------------------------------
    let mut send_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(payload) = pong_rx.recv() => {
                    if sender.send(Message::Pong(payload)).await.is_err() {
                        break;
                    }
                }
                msg = rx.recv() => {
                    match msg {
                        Some(m) => {
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
                        None => break,
                    }
                }
                else => break,
            }
        }
    });

    // --- Inbound pump ------------------------------------------------------
    let state2 = state.clone();
    let tx2 = tx.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(msg) = receiver.next().await {
            let m = match msg {
                Ok(m) => m,
                Err(_) => break,
            };
            match m {
                Message::Text(t) => {
                    // Route history replies to the raw parser FIRST: their
                    // bodies are relayed verbatim and must not be parsed
                    // into typed structs (8 MiB of base64 per part).
                    let kind = match serde_json::from_str::<TypePeek>(&t) {
                        Ok(p) => p.kind,
                        Err(_) => continue,
                    };
                    if kind == "history_response" {
                        if let Ok(hr) = serde_json::from_str::<HistoryResponseMsg>(&t) {
                            handle_history_response(
                                me_id_of(&state2, user_id),
                                session_id,
                                hr.to,
                                hr.messages,
                                hr.done,
                                &state2,
                            )
                            .await;
                        }
                        continue;
                    }
                    let cm: ClientMsg = match serde_json::from_str(&t) {
                        Ok(c) => c,
                        Err(_) => continue,
                    };
                    handle_client(cm, user_id, session_id, &state2, &tx2).await;
                }
                Message::Ping(payload) => {
                    let _ = pong_tx.send(payload);
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    // --- Teardown ----------------------------------------------------------
    // This session can no longer answer pulls addressed to it.
    state.clear_pulls_for_session(user_id, session_id);
    let now_empty = state.remove_session(user_id, session_id);
    if now_empty {
        state.clear_pulls_for_user(user_id);
        state.db.update_last_seen(user_id, now_ms()).await;
        let peers = state.db.list_all_relationships(user_id).await;
        for p in &peers {
            state.send_to_user(*p, ServerMsg::PeerOffline { user_id });
        }
        // Keep `user_status[user_id]` so the next reconnect restores it.
    }
}

// Small helper kept trivial for readability at the call site above.
fn me_id_of(_state: &AppState, user_id: i64) -> i64 {
    user_id
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
            state.user_status.insert(me_id, status);
            if let Some(mut sessions) = state.online.get_mut(&me_id) {
                for s in sessions.iter_mut() {
                    s.status = status;
                }
            }
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
            if let Some(name) = state.db.username(me_id).await {
                let _ = tx.send(ServerMsg::Profile {
                    user_id: me_id,
                    username: name.clone(),
                    display_name: display_name.clone(),
                    avatar: avatar.clone(),
                });
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
/// The "are they online?" check is on the SUBJECT of each notification,
/// not on the recipient.
async fn notify_peer_pair(state: &AppState, me_id: i64, to_id: i64) {
    let peers_for_me = state.db.list_peers(me_id).await;
    let _ = state.send_to_user(
        me_id,
        ServerMsg::Peers {
            peers: resolve_peers(state, &peers_for_me).await,
        },
    );
    let peers_for_to = state.db.list_peers(to_id).await;
    let _ = state.send_to_user(
        to_id,
        ServerMsg::Peers {
            peers: resolve_peers(state, &peers_for_to).await,
        },
    );
    if let Some((me_name, me_dn, me_av)) = state.db.get_user_profile(me_id).await {
        state.send_to_user(
            to_id,
            ServerMsg::Profile {
                user_id: me_id,
                username: me_name,
                display_name: me_dn,
                avatar: me_av,
            },
        );
    }
    if let Some((to_name, to_dn, to_av)) = state.db.get_user_profile(to_id).await {
        state.send_to_user(
            me_id,
            ServerMsg::Profile {
                user_id: to_id,
                username: to_name,
                display_name: to_dn,
                avatar: to_av,
            },
        );
    }
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

#[allow(clippy::too_many_arguments)]
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
    let ctx = state.db.send_context(me_id, to_id).await;
    if !ctx.exists {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", to_id),
        });
        return;
    }
    if ctx.blocked {
        return;
    }
    let deliverable = match ctx.relationship {
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
    let ctx = state.db.send_context(me_id, to_id).await;
    if !ctx.exists {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("user {} does not exist", to_id),
        });
        return;
    }
    if ctx.blocked {
        return;
    }
    if !matches!(ctx.relationship, Some((_, true))) {
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
    // Forward to EVERY responder session (devices may hold different parts
    // of the history) and account one pull per delivered copy, so each
    // session's reply is accepted exactly once.
    let delivered = state.send_to_user_collect(
        from_id,
        ServerMsg::PullHistoryRequest {
            from: me_id,
            since,
            limit,
            before,
        },
    );
    if delivered.is_empty() {
        let _ = tx.send(ServerMsg::Error {
            msg: format!("peer {} is offline, try again later", from_id),
        });
        return;
    }
    for sid in delivered {
        state.note_pull(me_id, from_id, sid);
    }
}

async fn handle_history_response(
    me_id: i64,
    session_id: u64,
    to_id: i64,
    messages: Vec<Box<RawValue>>,
    done: bool,
    state: &AppState,
) {
    if me_id == to_id {
        return;
    }
    let ctx = state.db.send_context(me_id, to_id).await;
    if !ctx.exists || ctx.blocked {
        return;
    }
    let Some((initiator_id, established)) = ctx.relationship else {
        return;
    };
    if !established && initiator_id != me_id {
        return;
    }
    // Non-final parts only require an open pull; the final part consumes it.
    if done {
        if !state.take_pull(to_id, me_id, session_id) {
            return;
        }
    } else if !state.has_pull(to_id, me_id, session_id) {
        return;
    }
    if !established {
        state.db.establish_relationship(me_id, to_id).await;
        notify_peer_pair(state, me_id, to_id).await;
    }
    state.send_to_user(
        to_id,
        ServerMsg::HistoryResponse {
            from: me_id,
            messages,
            done,
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
    state
        .pulls
        .retain(|&(a, b, _), _| !((a == me_id && b == peer_id) || (a == peer_id && b == me_id)));
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
    let contacts = state.db.list_all_relationships(me_id).await;
    state.db.delete_user(me_id).await;
    state.online.remove(&me_id);
    state.user_status.remove(&me_id);
    state.clear_pulls_for_user(me_id);
    for p in contacts {
        state.send_to_user(p, ServerMsg::PeerOffline { user_id: me_id });
        state.send_to_user(p, ServerMsg::ChatLeft { peer: me_id });
    }
    let _ = tx.send(ServerMsg::Close {
        reason: "account_deleted".into(),
    });
}