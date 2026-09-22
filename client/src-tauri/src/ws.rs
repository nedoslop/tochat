use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::crypto;
use crate::db::Database;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};

pub async fn run(
    app: AppHandle,
    base_url: String,
    username: String,
    password: String,
    tx: mpsc::UnboundedSender<ClientMsg>,
    mut rx: mpsc::UnboundedReceiver<ClientMsg>,
    ready_tx: oneshot::Sender<Result<(), String>>,
    shutdown_rx: oneshot::Receiver<()>,
    db: Database,
) {
    let ws_url = to_ws_url(&base_url);
    let mut req = match ws_url.into_client_request() {
        Ok(r) => r,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("url: {e}")));
            return;
        }
    };
    let auth = format!("{username}:{password}");
    match HeaderValue::from_str(&auth) {
        Ok(v) => {
            req.headers_mut().insert("Authorization", v);
        }
        Err(e) => {
            let _ = ready_tx.send(Err(format!("header: {e}")));
            return;
        }
    }

    let (ws_stream, _) = match tokio_tungstenite::connect_async(req).await {
        Ok(x) => x,
        Err(e) => {
            let _ = ready_tx.send(Err(format!("ws: {e}")));
            return;
        }
    };
    let (mut write, mut read) = ws_stream.split();

    // First message must be AuthOk (or Error).
    let first = match read.next().await {
        Some(Ok(Message::Text(t))) => t.to_string(),
        _ => {
            let _ = ready_tx.send(Err("no auth response".into()));
            return;
        }
    };
    match serde_json::from_str::<ServerMsg>(&first) {
        Ok(ServerMsg::AuthOk {
            username: u,
            last_seen,
        }) => {
            let _ = app.emit("auth-ok", json!({ "username": u, "last_seen": last_seen }));
        }
        Ok(ServerMsg::Error { msg }) => {
            let _ = ready_tx.send(Err(msg));
            return;
        }
        Ok(_) => {
            let _ = ready_tx.send(Err("unexpected first message".into()));
            return;
        }
        Err(e) => {
            let _ = ready_tx.send(Err(format!("parse: {e}")));
            return;
        }
    }
    let _ = ready_tx.send(Ok(()));

    let mut write_task = tokio::spawn(async move {
        while let Some(cm) = rx.recv().await {
            let s = match serde_json::to_string(&cm) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if write.send(Message::Text(s.into())).await.is_err() {
                break;
            }
        }
        let _ = write.close().await;
    });

    let app2 = app.clone();
    let db2 = db.clone();
    let uname = username.clone();
    let tx2 = tx.clone();
    let mut read_task = tokio::spawn(async move {
        while let Some(Ok(m)) = read.next().await {
            match m {
                Message::Text(t) => {
                    if let Ok(sm) = serde_json::from_str::<ServerMsg>(&t.to_string()) {
                        handle_server_msg(&app2, &db2, &uname, &tx2, sm).await;
                    }
                }
                Message::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut write_task => { read_task.abort(); }
        _ = &mut read_task => { write_task.abort(); }
        _ = shutdown_rx => {
            write_task.abort();
            read_task.abort();
        }
    }

    let _ = app.emit("disconnected", ());
}

fn to_ws_url(base: &str) -> String {
    let base = base.trim_end_matches('/');
    let base = if let Some(r) = base.strip_prefix("https://") {
        format!("wss://{r}")
    } else if let Some(r) = base.strip_prefix("http://") {
        format!("ws://{r}")
    } else {
        base.to_string()
    };
    format!("{base}/login")
}

async fn handle_server_msg(
    app: &AppHandle,
    db: &Database,
    me: &str,
    tx: &mpsc::UnboundedSender<ClientMsg>,
    msg: ServerMsg,
) {
    match msg {
        ServerMsg::AuthOk { .. } => {}

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
            let (_, pw) = db.get_encryption(&from).await;
            let text = crypto::decrypt(&payload, pw.as_deref())
                .unwrap_or_else(|e| format!("[decryption failed: {e}]"));
            db.insert_message(&from, "in", ts, &text).await;
            let _ = app.emit(
                "message",
                json!({ "peer": from, "direction": "in", "ts": ts, "text": text }),
            );
        }

        ServerMsg::PullHistoryRequest { from, since } => {
            // Re-encrypt our stored messages with the *current* setting for
            // that peer before shipping them back.
            let (method, pw) = db.get_encryption(&from).await;
            let stored = db.get_messages_since(&from, since).await;
            let mut out = Vec::with_capacity(stored.len());
            for m in stored {
                let payload = match crypto::encrypt(&method, pw.as_deref(), &m.text) {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                let (m_from, m_to) = if m.direction == "out" {
                    (me.to_string(), from.clone())
                } else {
                    (from.clone(), me.to_string())
                };
                out.push(StoredMsg {
                    from: m_from,
                    to: m_to,
                    ts: m.ts,
                    payload,
                });
            }
            let _ = tx.send(ClientMsg::HistoryResponse {
                to: from,
                messages: out,
            });
        }

        ServerMsg::HistoryResponse { from, messages } => {
            let (_, pw) = db.get_encryption(&from).await;
            for m in &messages {
                let text = crypto::decrypt(&m.payload, pw.as_deref())
                    .unwrap_or_else(|e| format!("[decryption failed: {e}]"));
                let direction = if m.from == me { "out" } else { "in" };
                db.insert_message(&from, direction, m.ts, &text).await;
            }
            let _ = app.emit("history-received", &from);
        }

        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }
        ServerMsg::Close => {
            let _ = app.emit("session-closed", ());
        }
    }
}
