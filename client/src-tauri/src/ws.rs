use tauri::{AppHandle, Emitter, Manager};

use crate::crypto;
use crate::protocol::{ClientMsg, ServerMsg, UiMsg};
use crate::state::AppState;

/// Send a message through the active websocket, if any.
pub async fn send(app: &AppHandle, msg: ClientMsg) {
    let state = app.state::<AppState>();
    let tx = state.ws.lock().await.tx.clone();
    if let Some(tx) = tx {
        let _ = tx.send(msg);
    }
}

async fn last_seen(app: &AppHandle) -> i64 {
    app.state::<AppState>().ws.lock().await.last_seen
}

/// Handle one decoded server frame.
pub async fn handle_server(sm: ServerMsg, app: &AppHandle, me: &str) {
    match sm {
        ServerMsg::AuthOk { username, last_seen: ls } => {
            {
                let state = app.state::<AppState>();
                state.ws.lock().await.last_seen = ls;
            }
            let _ = app.emit(
                "auth-ok",
                serde_json::json!({ "username": username, "last_seen": ls }),
            );
        }

        // Established peers. Pull from each in case they sent us something
        // while we were offline.
        ServerMsg::Peers { peers } => {
            let since = last_seen(app).await;
            for p in &peers {
                send(app, ClientMsg::PullHistory {
                    from: p.clone(),
                    since,
                })
                .await;
            }
            let _ = app.emit("peers", peers);
        }

        // Users who initiated a chat with us and whose first message we
        // have not pulled yet. UI shows them as "pending".
        ServerMsg::PendingChats { users } => {
            let _ = app.emit("pending-chats", users);
        }

        // A peer came online (or the relationship just got established).
        // Pull the history we missed.
        ServerMsg::PeerOnline { username } => {
            let since = last_seen(app).await;
            send(app, ClientMsg::PullHistory {
                from: username.clone(),
                since,
            })
            .await;
            let _ = app.emit("peer-online", username);
        }

        ServerMsg::PeerOffline { username } => {
            let _ = app.emit("peer-offline", username);
        }

        ServerMsg::Message { from, payload, ts } => {
            let state = app.state::<AppState>();
            let _ = state.db.store_message(&from, "in", ts, &payload).await;

            let pw = state.db.get_peer_password(&from).await;
            let text = pw
                .as_deref()
                .and_then(|p| crypto::decrypt(p, &payload))
                .unwrap_or_else(|| format!("<encrypted: {payload}>"));

            let _ = app.emit(
                "message",
                UiMsg {
                    peer: from,
                    direction: "in".into(),
                    ts,
                    text,
                },
            );
        }

        // A peer wants our local history with them, since `since`.
        // We answer directly from our local DB.
        ServerMsg::PullHistoryRequest { from, since } => {
            let msgs = {
                let state = app.state::<AppState>();
                state
                    .db
                    .history_for_peer(&from, me, since)
                    .await
                    .unwrap_or_default()
            };
            send(
                app,
                ClientMsg::HistoryResponse {
                    to: from,
                    messages: msgs,
                },
            )
            .await;
        }

        // A peer's answer to a pull we made. Store new rows, then emit them.
        ServerMsg::HistoryResponse { from, messages } => {
            let mut fresh: Vec<(i64, String, String)> = Vec::new(); // ts, payload, direction
            {
                let state = app.state::<AppState>();
                for m in &messages {
                    let direction = if m.from == me { "out" } else { "in" };
                    let is_new = state
                        .db
                        .store_message(&from, direction, m.ts, &m.payload)
                        .await
                        .unwrap_or(false);
                    if is_new {
                        fresh.push((m.ts, m.payload.clone(), direction.to_string()));
                    }
                }
            }

            let state = app.state::<AppState>();
            let pw = state.db.get_peer_password(&from).await;
            for (ts, payload, direction) in fresh {
                let text = pw
                    .as_deref()
                    .and_then(|p| crypto::decrypt(p, &payload))
                    .unwrap_or_else(|| format!("<encrypted: {payload}>"));
                let _ = app.emit(
                    "message",
                    UiMsg {
                        peer: from.clone(),
                        direction,
                        ts,
                        text,
                    },
                );
            }
        }

        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }

        // Server-initiated close: we were deleted, or someone else took the
        // single allowed session for this user.
        ServerMsg::Close => {
            {
                let state = app.state::<AppState>();
                let mut ws = state.ws.lock().await;
                ws.tx = None;
                ws.username = None;
            }
            let _ = app.emit("session-closed", ());
        }
    }
}