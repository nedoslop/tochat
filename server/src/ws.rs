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
use crate::util::{hash_password, now_ms};

static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// HTTP handler: verifies credentials then upgrades the request to a WebSocket.
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

/// Builds a 401 Unauthorized response.
fn reject(msg: &str) -> axum::response::Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({ "error": "unauthorized", "message": msg })),
    )
        .into_response()
}

/// Runs the WebSocket session: handshake, send/recv pumps, cleanup.
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

    // Take over any existing session for this user.
    match state.online.entry(username.clone()) {
        Entry::Occupied(mut e) => {
            let old = e.get();
            let _ = old.tx.send(ServerMsg::Close);
            e.insert(OnlineSession { tx: tx.clone(), session_id });
        }
        Entry::Vacant(v) => {
            v.insert(OnlineSession { tx: tx.clone(), session_id });
        }
    }

    // Handshake.
    let _ = tx.send(ServerMsg::AuthOk { username: username.clone(), last_seen });
    let peers = state.db.list_peers(user_id).await;
    let pending = state.db.list_pending_chats(user_id).await;
    let _ = tx.send(ServerMsg::Peers { peers: peers.clone() });
    let _ = tx.send(ServerMsg::PendingChats { users: pending });

    // Cross-notify online peers.
    for p in &peers {
        if state.online.contains_key(p) {
            let _ = tx.send(ServerMsg::PeerOnline { username: p.clone() });
        }
        if let Some(peer) = state.online.get(p) {
            let _ = peer
                .tx
                .send(ServerMsg::PeerOnline { username: username.clone() });
        }
    }

    // Writer: forward queued ServerMsg to the socket.
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

    // Reader: parse ClientMsg and dispatch.
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
            handle_client(cm, user_id, &username2, &state2, &tx2).await;
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    // Cleanup only if this session is still the active one.
    let still_current = state
        .online
        .get(&username)
        .map(|e| e.session_id == session_id)
        .unwrap_or(false);
    if still_current {
        state.online.remove(&username);
        state.db.update_last_seen(user_id, now_ms()).await;
        // Re-query peers in case new ones appeared during the session.
        let peers = state.db.list_peers(user_id).await;
        for p in &peers {
            if let Some(peer) = state.online.get(p) {
                let _ = peer.tx.send(ServerMsg::PeerOffline {
                    username: username.clone(),
                });
            }
        }
    }
}

/// Dispatches a single client message.
async fn handle_client(
    cm: ClientMsg,
    me_id: i64,
    me: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match cm {
        ClientMsg::Send { to, id, ts, kind, payload } => {
            handle_send(me_id, me, &to, id, ts, kind, payload, state, tx).await
        }
        ClientMsg::Edit { to, id, ts, edit_ts, kind, payload } => {
            handle_edit(me_id, me, &to, id, ts, edit_ts, kind, payload, state, tx).await
        }
        ClientMsg::PullHistory { from, since } => {
            handle_pull_history(me_id, me, &from, since, state, tx).await
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
    }
}

/// After a relationship becomes established, notify both sides (if online).
async fn notify_peer_pair(state: &AppState, me: &str, to: &str) {
    let to_online = state.online.contains_key(to);
    let me_online = state.online.contains_key(me);

    if to_online {
        if let Some(peer) = state.online.get(to) {
            let _ = peer.tx.send(ServerMsg::PeerOnline { username: me.to_string() });
        }
    }
    if to_online && me_online {
        if let Some(my) = state.online.get(me) {
            let _ = my.tx.send(ServerMsg::PeerOnline { username: to.to_string() });
        }
    }
    // If `to` is offline, we deliberately say nothing about them.
}

/// Handles a new message from `me` to `to`.
async fn handle_send(
    me_id: i64,
    me: &str,
    to: &str,
    id: String,
    ts: i64,
    kind: String,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == to {
        let _ = tx.send(ServerMsg::Error { msg: "cannot send to yourself".into() });
        return;
    }
    if payload.is_empty() {
        let _ = tx.send(ServerMsg::Error { msg: "empty message".into() });
        return;
    }
    let Some(to_id) = state.db.user_id(to).await else {
        let _ = tx.send(ServerMsg::Error { msg: format!("user '{}' does not exist", to) });
        return;
    };

    let deliverable = match state.db.get_relationship(me_id, to_id).await {
        None => {
            // First contact: create a pending relationship, don't deliver.
            state.db.ensure_relationship_initiated(me_id, to_id).await;
            false
        }
        Some((initiator_id, established)) => {
            let implicit_accept = !established && initiator_id != me_id;
            if implicit_accept {
                state.db.establish_relationship(me_id, to_id).await;
                notify_peer_pair(state, me, to).await;
            }
            established || implicit_accept
        }
    };

    if deliverable {
        if let Some(peer) = state.online.get(to) {
            let _ = peer.tx.send(ServerMsg::Message {
                id,
                from: me.to_string(),
                ts,
                edit_ts: ts,
                kind,
                payload,
            });
        }
    }
    // If not deliverable, the peer will pull history once accepted.
}

/// Handles an edit/delete of an existing message.
async fn handle_edit(
    me_id: i64,
    me: &str,
    to: &str,
    id: String,
    ts: i64,
    edit_ts: i64,
    kind: String,
    payload: String,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == to {
        return;
    }
    let Some(to_id) = state.db.user_id(to).await else {
        let _ = tx.send(ServerMsg::Error { msg: format!("user '{}' does not exist", to) });
        return;
    };

    // Only relay edits on established relationships; otherwise the peer
    // hasn't received the original message yet and will get the latest
    // version when they pull.
    let established = matches!(state.db.get_relationship(me_id, to_id).await, Some((_, true)));
    if !established {
        return;
    }

    if let Some(peer) = state.online.get(to) {
        let _ = peer.tx.send(ServerMsg::Message {
            id,
            from: me.to_string(),
            ts,
            edit_ts,
            kind,
            payload,
        });
    }
}

/// Forwards a history pull request to a peer if the relationship allows it.
async fn handle_pull_history(
    me_id: i64,
    me: &str,
    from: &str,
    since: i64,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    if me == from {
        let _ = tx.send(ServerMsg::Error { msg: "cannot pull from yourself".into() });
        return;
    }
    let Some(from_id) = state.db.user_id(from).await else {
        let _ = tx.send(ServerMsg::Error { msg: format!("user '{}' does not exist", from) });
        return;
    };
    let Some((initiator_id, established)) = state.db.get_relationship(me_id, from_id).await else {
        let _ = tx.send(ServerMsg::Error { msg: format!("no chat with {}", from) });
        return;
    };
    // While pending, only the initiator's history may be pulled.
    if !established && initiator_id != from_id {
        let _ = tx.send(ServerMsg::Error { msg: format!("{} has not messaged you", from) });
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

/// Delivers a history response to the requester, establishing if needed.
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
    let Some(to_id) = state.db.user_id(to).await else { return };

    if let Some((_, established)) = state.db.get_relationship(me_id, to_id).await {
        if !established {
            state.db.establish_relationship(me_id, to_id).await;
            notify_peer_pair(state, me, to).await;
        }
    }
    if let Some(peer) = state.online.get(to) {
        let _ = peer.tx.send(ServerMsg::HistoryResponse {
            from: me.to_string(),
            messages,
        });
    }
}

/// Deletes the caller's account after verifying their password.
async fn handle_delete_account(
    me_id: i64,
    me: &str,
    password: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    let Some((id, h, _)) = state.db.get_user(me).await else {
        let _ = tx.send(ServerMsg::Error { msg: "unknown user".into() });
        return;
    };
    if id != me_id || h != hash_password(password) {
        let _ = tx.send(ServerMsg::Error { msg: "invalid password".into() });
        return;
    }
    let peers = state.db.list_peers(me_id).await;
    for p in &peers {
        if let Some(peer) = state.online.get(p) {
            let _ = peer.tx.send(ServerMsg::PeerOffline {
                username: me.to_string(),
            });
        }
    }
    state.db.delete_user(me_id).await;
    state.online.remove(me);
    let _ = tx.send(ServerMsg::Close);
}