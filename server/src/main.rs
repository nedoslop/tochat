use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Router,
};
use dashmap::DashMap;
use futures_util::{sink::SinkExt, stream::StreamExt};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

#[derive(Clone)]
struct AppState {
    db: Arc<Mutex<Connection>>,
    online: Arc<DashMap<String, mpsc::UnboundedSender<ServerMsg>>>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Auth { username: String, password: String },
    Send { to: String, payload: String },
    HistoryRequest { from: String, since: i64 },
    HistoryResponse { to: String, messages: Vec<StoredMsg> },
}

#[derive(Serialize, Deserialize, Clone)]
struct StoredMsg {
    from: String,
    to: String,
    ts: i64,
    payload: String,
}

#[derive(Serialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    AuthOk { username: String, last_seen: i64 },
    Peers { peers: Vec<String> },
    PeerOnline { username: String },
    Message { from: String, payload: String, ts: i64 },
    HistoryRequest { from: String, since: i64 },
    HistoryResponse { from: String, messages: Vec<StoredMsg> },
    Error { msg: String },
}

fn hash(pw: &str) -> String {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    format!("{:x}", h.finalize())
}

fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64
}

#[tokio::main]
async fn main() {
    let conn = Connection::open("chat.db").unwrap();
    conn.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS users (
            username TEXT PRIMARY KEY,
            password_hash TEXT NOT NULL,
            last_seen INTEGER NOT NULL DEFAULT 0
        );
        CREATE TABLE IF NOT EXISTS relationships (
            a TEXT NOT NULL,
            b TEXT NOT NULL,
            PRIMARY KEY (a, b)
        );
        "#,
    )
    .unwrap();

    let state = AppState {
        db: Arc::new(Mutex::new(conn)),
        online: Arc::new(DashMap::new()),
    };

    let app = Router::new().route("/ws", get(ws_handler)).with_state(state);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await.unwrap();
    println!("server listening on 0.0.0.0:8080");
    axum::serve(listener, app).await.unwrap();
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<AppState>) -> impl IntoResponse {
    ws.on_upgrade(|s| handle_socket(s, state))
}

async fn handle_socket(socket: WebSocket, state: AppState) {
    let (mut sender, mut receiver) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ServerMsg>();

    // --- auth ---
    let auth = match receiver.next().await {
        Some(Ok(Message::Text(t))) => serde_json::from_str::<ClientMsg>(&t).ok(),
        _ => None,
    };
    let (username, password) = match auth {
        Some(ClientMsg::Auth { username, password }) => (username, password),
        _ => {
            let _ = sender
                .send(Message::Text(
                    serde_json::to_string(&ServerMsg::Error {
                        msg: "auth required".into(),
                    })
                    .unwrap()
                    .into(),
                ))
                .await;
            return;
        }
    };

    let last_seen: i64 = {
        let db = state.db.lock().await;
        let existing: Option<(String, i64)> = db
            .query_row(
                "SELECT password_hash, last_seen FROM users WHERE username = ?1",
                [&username],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok();
        match existing {
            Some((h, ls)) => {
                if h != hash(&password) {
                    let _ = tx.send(ServerMsg::Error {
                        msg: "invalid credentials".into(),
                    });
                    return;
                }
                ls
            }
            None => {
                db.execute(
                    "INSERT INTO users (username, password_hash, last_seen) VALUES (?1, ?2, 0)",
                    rusqlite::params![&username, hash(&password)],
                )
                .unwrap();
                0
            }
        }
    };

    state.online.insert(username.clone(), tx.clone());
    let _ = tx.send(ServerMsg::AuthOk {
        username: username.clone(),
        last_seen,
    });

    // --- peers list + notify others this user is online ---
    let peers: Vec<String> = {
        let db = state.db.lock().await;
        let mut stmt = db
            .prepare(
                "SELECT CASE WHEN a = ?1 THEN b ELSE a END \
                 FROM relationships WHERE a = ?1 OR b = ?1",
            )
            .unwrap();
        let rows = stmt
            .query_map([&username], |r| r.get::<_, String>(0))
            .unwrap();
        rows.filter_map(|r| r.ok()).collect()
    };
    let _ = tx.send(ServerMsg::Peers { peers: peers.clone() });

    for p in &peers {
        if let Some(peer_tx) = state.online.get(p) {
            let _ = peer_tx.send(ServerMsg::PeerOnline {
                username: username.clone(),
            });
        }
    }

    // --- sender task ---
    let mut send_task = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let s = serde_json::to_string(&m).unwrap();
            if sender.send(Message::Text(s.into())).await.is_err() {
                break;
            }
        }
    });

    // --- receiver task ---
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

    // --- offline cleanup ---
    state.online.remove(&username);
    {
        let db = state.db.lock().await;
        let _ = db.execute(
            "UPDATE users SET last_seen = ?1 WHERE username = ?2",
            rusqlite::params![now(), &username],
        );
    }
}

async fn handle_client(
    cm: ClientMsg,
    me: &str,
    state: &AppState,
    tx: &mpsc::UnboundedSender<ServerMsg>,
) {
    match cm {
        ClientMsg::Auth { .. } => {}

        ClientMsg::Send { to, payload } => {
            let ts = now();
            // Первое сообщение создаёт relationship. Дальше она уже есть.
            {
                let db = state.db.lock().await;
                let _ = db.execute(
                    "INSERT OR IGNORE INTO relationships (a, b) VALUES (?1, ?2)",
                    rusqlite::params![me, &to],
                );
            }
            if let Some(peer) = state.online.get(&to) {
                let _ = peer.send(ServerMsg::Message {
                    from: me.to_string(),
                    payload,
                    ts,
                });
            }
            // Если пир офлайн — просто дропаем, отправитель хранит у себя.
        }

        ClientMsg::HistoryRequest { from, since } => {
            let allowed: bool = {
                let db = state.db.lock().await;
                db.query_row(
                    "SELECT COUNT(*) FROM relationships \
                     WHERE (a=?1 AND b=?2) OR (a=?2 AND b=?1)",
                    rusqlite::params![me, &from],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap_or(0)
                    > 0
            };
            if !allowed {
                let _ = tx.send(ServerMsg::Error {
                    msg: format!("no relationship with {}", from),
                });
                return;
            }
            match state.online.get(&from) {
                Some(peer) => {
                    let _ = peer.send(ServerMsg::HistoryRequest {
                        from: me.to_string(),
                        since,
                    });
                }
                None => {
                    let _ = tx.send(ServerMsg::Error {
                        msg: format!("peer {} is offline, will retry later", from),
                    });
                }
            }
        }

        ClientMsg::HistoryResponse { to, messages } => {
            if let Some(peer) = state.online.get(&to) {
                let _ = peer.send(ServerMsg::HistoryResponse {
                    from: me.to_string(),
                    messages,
                });
            }
        }
    }
}
