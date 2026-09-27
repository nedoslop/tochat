use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Emitter};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::protocol::{ClientMsg, ServerMsg, StoredMsg};
use crate::state::{AppState, WsHandle};

/// Converts an HTTP(S) base URL into a WS(S) URL for the given path.
fn to_ws_url(base_url: &str, path: &str) -> Result<String, String> {
    let trimmed = base_url.trim_end_matches('/');
    let replaced = if let Some(rest) = trimmed.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = trimmed.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        return Err("base_url must start with http:// or https://".into());
    };
    Ok(format!("{replaced}{path}"))
}

/// Opens a new WebSocket and spawns the send/recv pump.
pub async fn spawn(
    state: Arc<AppState>,
    base_url: &str,
    username: &str,
    password: &str,
) -> Result<(), String> {
    let ws_url = to_ws_url(base_url, "/login")?;
    let mut req = ws_url.into_client_request().map_err(|e| e.to_string())?;
    let auth = format!("{}:{}", username, password);
    req.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&auth).map_err(|e| e.to_string())?,
    );

    let (ws, _) = tokio_tungstenite::connect_async(req)
        .await
        .map_err(|e| format!("ws connect: {e}"))?;

    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();
    let shutdown = Arc::new(AtomicBool::new(false));

    let app = state.app.clone();
    let state_for_reader = state.clone();
    let me = username.to_string();
    let tx_for_reader = tx.clone();
    let shutdown_for_task = shutdown.clone();

    let task = tokio::spawn(async move {
        // Writer: ClientMsg -> socket.
        let writer_fut = async move {
            while let Some(m) = rx.recv().await {
                let s = match serde_json::to_string(&m) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                if sink.send(Message::Text(s.into())).await.is_err() {
                    break;
                }
            }
        };

        // Reader: socket -> ServerMsg -> handlers / events.
        let app_for_reader = app.clone();
        let reader_fut = async move {
            while let Some(Ok(msg)) = stream.next().await {
                let text = match msg {
                    Message::Text(t) => t.to_string(),
                    Message::Close(_) => break,
                    _ => continue,
                };
                let sm: ServerMsg = match serde_json::from_str(&text) {
                    Ok(s) => s,
                    Err(_) => continue,
                };
                handle_server_msg(
                    &app_for_reader,
                    &state_for_reader,
                    &me,
                    &tx_for_reader,
                    sm,
                )
                .await;
            }
        };

        tokio::pin!(writer_fut, reader_fut);
        tokio::select! {
            _ = &mut writer_fut => {},
            _ = &mut reader_fut => {},
        }

        // Only emit if we weren't deliberately shut down. This prevents a
        // stale task (from a previous login) from firing `disconnected`
        // after a new session has already started.
        if !shutdown_for_task.load(Ordering::Relaxed) {
            let _ = app.emit("disconnected", ());
        }
    });

    // Replace any existing handle.
    let mut guard = state.ws.lock().await;
    if let Some(old) = guard.take() {
        old.shutdown.store(true, Ordering::Relaxed);
        drop(old.tx);
        old.task.abort();
        let _ = old.task.await;
    }
    *guard = Some(WsHandle { tx, task, shutdown });
    Ok(())
}

/// Handles one ServerMsg: persists, emits Tauri events, auto-replies.
async fn handle_server_msg(
    app: &AppHandle,
    state: &Arc<AppState>,
    me: &str,
    tx: &mpsc::UnboundedSender<ClientMsg>,
    msg: ServerMsg,
) {
    match msg {
        ServerMsg::AuthOk { username, last_seen } => {
            let _ = app.emit(
                "auth-ok",
                serde_json::json!({ "username": username, "last_seen": last_seen }),
            );
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
        ServerMsg::Message { id, from, ts, edit_ts, kind, payload } => {
            if let Ok(db) = state.db().await {
                let stored = StoredMsg {
                    id: id.clone(),
                    from: from.clone(),
                    to: me.to_string(),
                    ts,
                    edit_ts,
                    kind: kind.clone(),
                    payload: payload.clone(),
                };
                db.upsert(&stored).await;
            }
            let _ = app.emit(
                "message",
                serde_json::json!({
                    "id": id,
                    "peer": from,
                    "direction": "in",
                    "ts": ts,
                    "edit_ts": edit_ts,
                    "kind": kind,
                    "payload": payload,
                }),
            );
        }
        ServerMsg::PullHistoryRequest { from, since } => {
            if let Ok(db) = state.db().await {
                let msgs = db.since_for_peer(&from, since).await;
                let _ = tx.send(ClientMsg::HistoryResponse { to: from, messages: msgs });
            }
        }
        ServerMsg::HistoryResponse { from, messages } => {
            if let Ok(db) = state.db().await {
                for m in &messages {
                    db.upsert(m).await;
                }
            }
            let _ = app.emit("history-received", from);
        }
        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }
        ServerMsg::Close { reason } => {
            let _ = app.emit("session-closed", reason);
        }
    }
}