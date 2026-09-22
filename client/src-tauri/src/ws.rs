use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message as WsMessage;

use crate::crypto;
use crate::db::LocalDb;
use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{AppState, EncConfig};

pub fn spawn_connection(
    app: AppHandle,
    state: AppState,
    db: LocalDb,
    base_url: String,
    username: String,
    password: String,
) -> Result<mpsc::UnboundedSender<ClientMsg>, String> {
    let (tx, rx) = mpsc::unbounded_channel::<ClientMsg>();
    let app2 = app.clone();
    let username2 = username.clone();

    tokio::spawn(async move {
        if let Err(e) = run(app2, state, db, base_url, username2, password, rx).await {
            eprintln!("ws error: {e}");
        }
    });

    Ok(tx)
}

async fn run(
    app: AppHandle,
    state: AppState,
    db: LocalDb,
    base_url: String,
    username: String,
    password: String,
    mut rx: mpsc::UnboundedReceiver<ClientMsg>,
) -> Result<(), String> {
    let ws_url = base_url
        .trim_end_matches('/')
        .replacen("https://", "wss://", 1)
        .replacen("http://", "ws://", 1);
    let url = format!("{}/login", ws_url);

    let mut req = url
        .into_client_request()
        .map_err(|e| format!("bad url: {e}"))?;
    let token = format!("{username}:{password}");
    req.headers_mut().insert(
        "Authorization",
        token.parse().map_err(|e| format!("bad header: {e}"))?,
    );

    let (stream, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let (mut sink, mut source) = stream.split();

    let mut send_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let s = match serde_json::to_string(&m) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if sink.send(WsMessage::Text(s.into())).await.is_err() {
                break;
            }
        }
    });

    let app_in = app.clone();
    let state_in = state.clone();
    let db_in = db.clone();
    let me = username.clone();
    let mut recv_task = tokio::spawn(async move {
        while let Some(Ok(msg)) = source.next().await {
            match msg {
                WsMessage::Text(t) => {
                    let parsed: ServerMsg = match serde_json::from_str(&t) {
                        Ok(p) => p,
                        Err(_) => continue,
                    };
                    handle_server_msg(&app_in, &state_in, &db_in, &me, parsed).await;
                }
                WsMessage::Close(_) => break,
                _ => {}
            }
        }
    });

    tokio::select! {
        _ = &mut send_task => recv_task.abort(),
        _ = &mut recv_task => send_task.abort(),
    }

    let _ = app.emit("disconnected", ());
    Ok(())
}

async fn handle_server_msg(
    app: &AppHandle,
    state: &AppState,
    db: &LocalDb,
    me: &str,
    msg: ServerMsg,
) {
    match msg {
        ServerMsg::AuthOk { username, last_seen } => {
            let _ = app.emit(
                "auth-ok",
                json!({ "username": username, "lastSeen": last_seen }),
            );
        }
        ServerMsg::Peers { peers } => {
            let _ = app.emit("peers", peers.clone());
            // Auto-sync with every peer.
            for p in peers {
                send_pull(state, &p);
            }
        }
        ServerMsg::PendingChats { users } => {
            let _ = app.emit("pending-chats", users);
        }
        ServerMsg::PeerOnline { username } => {
            let _ = app.emit("peer-online", username.clone());
            // Sync from peer the moment they come online.
            send_pull(state, &username);
        }
        ServerMsg::PeerOffline { username } => {
            let _ = app.emit("peer-offline", username);
        }
        ServerMsg::Message { from, payload, ts } => {
            let text = decrypt_payload(state, &from, &payload);
            let _ = db.insert_message_dedup(&from, "in", ts, &text).await;
            let _ = app.emit(
                "message",
                json!({ "peer": from, "direction": "in", "ts": ts, "text": text }),
            );
        }
        ServerMsg::PullHistoryRequest { from, since: _ } => {
            // Peer wants our log with them → send everything we have.
            let msgs = db.get_messages(&from).await;
            let stored: Vec<StoredMsg> = msgs
                .into_iter()
                .map(|(direction, ts, text)| StoredMsg {
                    from: if direction == "out" { me.to_string() } else { from.clone() },
                    to: if direction == "out" { from.clone() } else { me.to_string() },
                    ts,
                    payload: text,
                })
                .collect();
            let stored = encrypt_history(state, &from, stored);
            send_client(state, ClientMsg::HistoryResponse { to: from, messages: stored });
        }
        ServerMsg::HistoryResponse { from, messages } => {
            for m in messages {
                let (peer, direction, raw) = if m.from == me {
                    (m.to.clone(), "out", m.payload)
                } else {
                    (m.from.clone(), "in", m.payload)
                };
                let text = decrypt_payload(state, &peer, &raw);
                let _ = db.insert_message_dedup(&peer, direction, m.ts, &text).await;
            }
            let _ = app.emit("history-received", from);
        }
        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }
        ServerMsg::Close => {
            let _ = app.emit("session-closed", ());
        }
    }
}

fn send_client(state: &AppState, m: ClientMsg) {
    if let Some(conn) = state.conn.lock().unwrap().as_ref() {
        let _ = conn.tx.send(m);
    }
}

fn send_pull(state: &AppState, peer: &str) {
    send_client(
        state,
        ClientMsg::PullHistory {
            from: peer.to_string(),
            since: 0,
        },
    );
}

fn decrypt_payload(state: &AppState, peer: &str, payload: &str) -> String {
    let cfg = {
        let map = state.enc.lock().unwrap();
        map.get(peer).cloned()
    };
    if let Some(cfg) = cfg {
        if cfg.method == "aes-gcm" {
            if let Some(pw) = cfg.password {
                if let Ok(bytes) = crypto::decrypt(payload, &pw) {
                    if let Ok(s) = String::from_utf8(bytes) {
                        return s;
                    }
                }
            }
        }
    }
    payload.to_string()
}

fn encrypt_history(state: &AppState, peer: &str, msgs: Vec<StoredMsg>) -> Vec<StoredMsg> {
    let cfg: Option<EncConfig> = {
        let map = state.enc.lock().unwrap();
        map.get(peer).cloned()
    };
    let Some(cfg) = cfg else { return msgs };
    if cfg.method != "aes-gcm" {
        return msgs;
    }
    let Some(pw) = cfg.password else { return msgs };
    msgs.into_iter()
        .map(|m| {
            let enc = crypto::encrypt(m.payload.as_bytes(), &pw).unwrap_or_else(|_| m.payload.clone());
            StoredMsg {
                from: m.from,
                to: m.to,
                ts: m.ts,
                payload: enc,
            }
        })
        .collect()
}