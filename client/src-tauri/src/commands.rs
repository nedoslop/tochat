use futures_util::{SinkExt, StreamExt};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;

use crate::crypto;
use crate::http_auth;
use crate::protocol::{ClientMsg, UiMsg};
use crate::state::AppState;
use crate::util::now;
use crate::ws;

#[tauri::command]
pub async fn connect(
    app: AppHandle,
    base_url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    // 1. HTTP login (or register) first.
    http_auth::login_or_register(&base_url, &username, &password).await?;

    // 2. Build the WS upgrade request the canonical way.
    let ws_url = http_auth::to_ws_url(&base_url)?;

    // `into_client_request` on a String/Uri injects Sec-WebSocket-Key,
    // Sec-WebSocket-Version, Upgrade and Connection for us. Building a
    // `http::Request<()>` by hand does *not*, which is why the server
    // complained about the missing sec-websocket-key header.
    let mut request = ws_url
        .into_client_request()
        .map_err(|e| format!("bad websocket URL: {e}"))?;

    request.headers_mut().insert(
        "Authorization",
        HeaderValue::from_str(&format!("{username}:{password}"))
            .map_err(|e| format!("bad credentials header: {e}"))?,
    );

    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| format!("websocket connect failed: {e}"))?;

    let (mut write, mut read) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();

    // 3. Store the sender so `send_message` etc. can use it.
    {
        let state = app.state::<AppState>();
        let mut ws = state.ws.lock().await;
        ws.tx = Some(tx.clone());
        ws.username = Some(username.clone());
        ws.last_seen = 0;
    }

    // 4. Writer task.
    tauri::async_runtime::spawn(async move {
        while let Some(m) = rx.recv().await {
            let s = match serde_json::to_string(&m) {
                Ok(s) => s,
                Err(_) => continue,
            };
            if write.send(Message::Text(s)).await.is_err() {
                break;
            }
        }
    });

    // 5. Reader task.
    let app_reader = app.clone();
    let username_reader = username.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if let Message::Text(t) = msg {
                if let Ok(sm) = serde_json::from_str(&t) {
                    ws::handle_server(sm, &app_reader, &username_reader).await;
                }
            }
        }
        {
            let state = app_reader.state::<AppState>();
            let mut ws = state.ws.lock().await;
            ws.tx = None;
            ws.username = None;
        }
        let _ = app_reader.emit("disconnected", ());
    });

    Ok(())
}

#[tauri::command]
pub async fn send_message(app: AppHandle, to: String, text: String) -> Result<(), String> {
    let state = app.state::<AppState>();

    let pw = state
        .db
        .get_peer_password(&to)
        .await
        .ok_or_else(|| format!("set the shared password for {to} first"))?;

    let payload = crypto::encrypt(&pw, &text);
    let ts = now();

    // Persist locally before sending, so the UI can always show it.
    state
        .db
        .store_message(&to, "out", ts, &payload)
        .await
        .map_err(|e| e.to_string())?;

    let tx = state.ws.lock().await.tx.clone().ok_or("not connected")?;
    tx.send(ClientMsg::Send {
        to: to.clone(),
        payload,
    })
    .map_err(|e| e.to_string())?;

    let _ = app.emit(
        "message",
        UiMsg {
            peer: to,
            direction: "out".into(),
            ts,
            text,
        },
    );
    Ok(())
}

/// Ask the server to forward a history pull to `from`.
/// Only allowed by the server if `from` initiated the chat with us.
#[tauri::command]
pub async fn pull_history(app: AppHandle, from: String) -> Result<(), String> {
    let (since, tx) = {
        let state = app.state::<AppState>();
        let guard = state.ws.lock().await;
        // Bind the values into locals so the guard and `state` are
        // dropped *before* the block's tail expression is evaluated.
        let since = guard.last_seen;
        let tx = guard.tx.clone();
        (since, tx)
    };

    let tx = tx.ok_or("not connected")?;
    tx.send(ClientMsg::PullHistory { from, since })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Ask the server to re-send the list of pending chats.
#[tauri::command]
pub async fn list_pending(app: AppHandle) -> Result<(), String> {
    let tx = {
        let state = app.state::<AppState>();
        let guard = state.ws.lock().await;
        let tx = guard.tx.clone();
        tx
    };

    let tx = tx.ok_or("not connected")?;
    tx.send(ClientMsg::ListPending).map_err(|e| e.to_string())?;
    Ok(())
}

/// Request account deletion. Server confirms by sending a `Close` frame.
#[tauri::command]
pub async fn delete_account(app: AppHandle, password: String) -> Result<(), String> {
    let tx = {
        let state = app.state::<AppState>();
        let guard = state.ws.lock().await;
        let tx = guard.tx.clone();
        tx
    };

    let tx = tx.ok_or("not connected")?;
    tx.send(ClientMsg::DeleteAccount { password })
        .map_err(|e| e.to_string())?;
    Ok(())
}

/// Drop the websocket (log out). The reader task will emit `disconnected`.
#[tauri::command]
pub async fn disconnect(app: AppHandle) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut ws = state.ws.lock().await;
    ws.tx = None;
    ws.username = None;
    Ok(())
}

#[tauri::command]
pub async fn set_peer_password(
    state: State<'_, AppState>,
    peer: String,
    password: String,
) -> Result<(), String> {
    state
        .db
        .set_peer_password(&peer, &password)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_messages(state: State<'_, AppState>, peer: String) -> Result<Vec<UiMsg>, String> {
    let pw = state.db.get_peer_password(&peer).await;
    let rows = state
        .db
        .messages_for_peer(&peer)
        .await
        .map_err(|e| e.to_string())?;

    let mut out = Vec::with_capacity(rows.len());
    for (direction, ts, payload) in rows {
        let text = pw
            .as_deref()
            .and_then(|p| crypto::decrypt(p, &payload))
            .unwrap_or_else(|| format!("<encrypted: {payload}>"));
        out.push(UiMsg {
            peer: peer.clone(),
            direction,
            ts,
            text,
        });
    }
    Ok(out)
}

/// Wipe local DB after account deletion (UI calls this after `session-closed`).
#[tauri::command]
pub async fn wipe_local_data(state: State<'_, AppState>) -> Result<(), String> {
    state.db.wipe().await.map_err(|e| e.to_string())
}
