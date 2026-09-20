#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as B64, Engine};
use futures_util::{SinkExt, StreamExt};
use rand::RngCore;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter, Manager, State};
use tokio::sync::{mpsc, Mutex};
use tokio_tungstenite::tungstenite::{handshake::client::Request, Message};

// ---------- state ----------

struct AppState {
    db: Mutex<Connection>,
    ws_tx: Mutex<Option<mpsc::UnboundedSender<ClientMsg>>>,
    last_seen: Mutex<i64>,
}

// ---------- protocol ----------

#[derive(Serialize, Deserialize, Clone)]
struct StoredMsg {
    from: String,
    to: String,
    ts: i64,
    payload: String,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMsg {
    Send {
        to: String,
        payload: String,
    },
    HistoryRequest {
        from: String,
        since: i64,
    },
    HistoryResponse {
        to: String,
        messages: Vec<StoredMsg>,
    },
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerMsg {
    AuthOk {
        username: String,
        last_seen: i64,
    },
    Peers {
        peers: Vec<String>,
    },
    PeerOnline {
        username: String,
    },
    Message {
        from: String,
        payload: String,
        ts: i64,
    },
    HistoryRequest {
        from: String,
        since: i64,
    },
    HistoryResponse {
        from: String,
        messages: Vec<StoredMsg>,
    },
    Error {
        msg: String,
    },
}

#[derive(Serialize, Clone)]
struct UiMsg {
    peer: String,
    direction: String, // "in" | "out"
    ts: i64,
    text: String,
}

// ---------- crypto ----------

fn key_from_password(pw: &str) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(pw.as_bytes());
    let r = h.finalize();
    let mut k = [0u8; 32];
    k.copy_from_slice(&r);
    k
}

fn encrypt(pw: &str, plaintext: &str) -> String {
    let key = key_from_password(pw);
    let cipher = Aes256Gcm::new((&key).into());
    let mut nonce_bytes = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ct = cipher.encrypt(nonce, plaintext.as_bytes()).unwrap();
    let mut out = nonce_bytes.to_vec();
    out.extend_from_slice(&ct);
    B64.encode(&out)
}

fn decrypt(pw: &str, payload: &str) -> Option<String> {
    let key = key_from_password(pw);
    let cipher = Aes256Gcm::new((&key).into());
    let data = B64.decode(payload).ok()?;
    if data.len() < 12 {
        return None;
    }
    let (nonce_b, ct) = data.split_at(12);
    let nonce = Nonce::from_slice(nonce_b);
    let pt = cipher.decrypt(nonce, ct).ok()?;
    String::from_utf8(pt).ok()
}

// ---------- helpers ----------

async fn get_peer_pw(state: &AppState, peer: &str) -> Option<String> {
    let db = state.db.lock().await;
    db.query_row(
        "SELECT password FROM peer_settings WHERE peer=?1",
        [peer],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

// ---------- commands ----------

#[tauri::command]
async fn connect(
    app: AppHandle,
    url: String,
    username: String,
    password: String,
) -> Result<(), String> {
    let concat_str = format!("{}:{}", username, password);
    let request = Request::builder()
        .uri(url)
        .header("Authorization", concat_str)
        .body(()) // The body must be empty (unit type `()`) for a WebSocket handshake
        .map_err(|e| e.to_string())?;

    // 2. Pass the request instead of the plain URL
    let (ws, _) = tokio_tungstenite::connect_async(request)
        .await
        .map_err(|e| e.to_string())?;

    let (mut write, mut read) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientMsg>();

    {
        let state = app.state::<AppState>();
        *state.ws_tx.lock().await = Some(tx.clone());
    }

    // writer
    tauri::async_runtime::spawn(async move {
        while let Some(m) = rx.recv().await {
            let s = serde_json::to_string(&m).unwrap();
            if write.send(Message::Text(s)).await.is_err() {
                break;
            }
        }
    });

    // reader
    let app2 = app.clone();
    let username2 = username.clone();
    tauri::async_runtime::spawn(async move {
        while let Some(Ok(msg)) = read.next().await {
            if let Message::Text(t) = msg {
                if let Ok(sm) = serde_json::from_str::<ServerMsg>(&t) {
                    handle_server(sm, &app2, &username2).await;
                }
            }
        }
        let _ = app2.emit("disconnected", ());
    });

    Ok(())
}

async fn handle_server(sm: ServerMsg, app: &AppHandle, me: &str) {
    let state = app.state::<AppState>();
    match sm {
        ServerMsg::AuthOk {
            username,
            last_seen,
        } => {
            *state.last_seen.lock().await = last_seen;
            let _ = app.emit(
                "auth-ok",
                serde_json::json!({ "username": username, "last_seen": last_seen }),
            );
        }

        ServerMsg::Peers { peers } => {
            let since = *state.last_seen.lock().await;
            let tx = state.ws_tx.lock().await.clone();
            if let Some(tx) = tx {
                for p in &peers {
                    let _ = tx.send(ClientMsg::HistoryRequest {
                        from: p.clone(),
                        since,
                    });
                }
            }
            let _ = app.emit("peers", peers);
        }

        ServerMsg::PeerOnline { username } => {
            let since = *state.last_seen.lock().await;
            let tx = state.ws_tx.lock().await.clone();
            if let Some(tx) = tx {
                let _ = tx.send(ClientMsg::HistoryRequest {
                    from: username,
                    since,
                });
            }
        }

        ServerMsg::Message { from, payload, ts } => {
            {
                let db = state.db.lock().await;
                let _ = db.execute(
                    "INSERT OR IGNORE INTO messages (peer, direction, ts, payload) \
                     VALUES (?1, 'in', ?2, ?3)",
                    rusqlite::params![&from, ts, &payload],
                );
            }
            let pw = get_peer_pw(&state, &from).await;
            let text = pw
                .as_deref()
                .and_then(|p| decrypt(p, &payload))
                .unwrap_or_else(|| format!("<encrypted: {}>", payload));
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

        ServerMsg::HistoryRequest { from, since } => {
            let msgs: Vec<StoredMsg> = {
                let db = state.db.lock().await;
                let mut stmt = db
                    .prepare(
                        "SELECT direction, ts, payload FROM messages \
                         WHERE peer=?1 AND ts > ?2 ORDER BY ts ASC",
                    )
                    .unwrap();
                let rows = stmt
                    .query_map(rusqlite::params![&from, since], |r| {
                        let direction: String = r.get(0)?;
                        let ts: i64 = r.get(1)?;
                        let payload: String = r.get(2)?;
                        Ok((direction, ts, payload))
                    })
                    .unwrap();
                rows.filter_map(|r| r.ok())
                    .map(|(direction, ts, payload)| StoredMsg {
                        from: if direction == "in" {
                            from.clone()
                        } else {
                            me.to_string()
                        },
                        to: if direction == "in" {
                            me.to_string()
                        } else {
                            from.clone()
                        },
                        ts,
                        payload,
                    })
                    .collect()
            };
            let tx = state.ws_tx.lock().await.clone();
            if let Some(tx) = tx {
                let _ = tx.send(ClientMsg::HistoryResponse {
                    to: from,
                    messages: msgs,
                });
            }
        }

        ServerMsg::HistoryResponse { from, messages } => {
            let mut new_msgs: Vec<(i64, String)> = Vec::new();
            {
                let db = state.db.lock().await;
                for m in &messages {
                    let direction = if m.from == me { "out" } else { "in" };
                    let n = db
                        .execute(
                            "INSERT OR IGNORE INTO messages (peer, direction, ts, payload) \
                             VALUES (?1, ?2, ?3, ?4)",
                            rusqlite::params![&from, direction, m.ts, &m.payload],
                        )
                        .unwrap_or(0);
                    if n > 0 {
                        new_msgs.push((m.ts, m.payload.clone()));
                    }
                }
            }
            let pw = get_peer_pw(&state, &from).await;
            for (ts, payload) in new_msgs {
                let text = pw
                    .as_deref()
                    .and_then(|p| decrypt(p, &payload))
                    .unwrap_or_else(|| format!("<encrypted: {}>", payload));
                let _ = app.emit(
                    "message",
                    UiMsg {
                        peer: from.clone(),
                        direction: "in".into(),
                        ts,
                        text,
                    },
                );
            }
        }

        ServerMsg::Error { msg } => {
            let _ = app.emit("error", msg);
        }
    }
}

#[tauri::command]
async fn send_message(app: AppHandle, to: String, text: String) -> Result<(), String> {
    let state = app.state::<AppState>();
    let pw = get_peer_pw(&state, &to)
        .await
        .ok_or_else(|| format!("set the shared password for {} first", to))?;

    let payload = encrypt(&pw, &text);
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as i64;

    {
        let db = state.db.lock().await;
        db.execute(
            "INSERT INTO messages (peer, direction, ts, payload) VALUES (?1, 'out', ?2, ?3)",
            rusqlite::params![&to, ts, &payload],
        )
        .map_err(|e| e.to_string())?;
    }

    let tx = state.ws_tx.lock().await.clone().ok_or("not connected")?;
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

#[tauri::command]
async fn set_peer_password(
    state: State<'_, AppState>,
    peer: String,
    password: String,
) -> Result<(), String> {
    let db = state.db.lock().await;
    db.execute(
        "INSERT INTO peer_settings (peer, password) VALUES (?1, ?2) \
         ON CONFLICT(peer) DO UPDATE SET password=excluded.password",
        rusqlite::params![peer, password],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
async fn get_messages(state: State<'_, AppState>, peer: String) -> Result<Vec<UiMsg>, String> {
    let pw: Option<String> = {
        let db = state.db.lock().await;
        db.query_row(
            "SELECT password FROM peer_settings WHERE peer=?1",
            [&peer],
            |r| r.get::<_, String>(0),
        )
        .ok()
    };
    let db = state.db.lock().await;
    let mut stmt = db
        .prepare("SELECT direction, ts, payload FROM messages WHERE peer=?1 ORDER BY ts ASC")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([&peer], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        let (direction, ts, payload) = row.map_err(|e| e.to_string())?;
        let text = pw
            .as_deref()
            .and_then(|p| decrypt(p, &payload))
            .unwrap_or_else(|| format!("<encrypted: {}>", payload));
        out.push(UiMsg {
            peer: peer.clone(),
            direction,
            ts,
            text,
        });
    }
    Ok(out)
}

// ---------- main ----------

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir().unwrap();
            std::fs::create_dir_all(&dir).unwrap();
            let conn = Connection::open(dir.join("local.db")).unwrap();
            conn.execute_batch(
                r#"
                CREATE TABLE IF NOT EXISTS peer_settings (
                    peer TEXT PRIMARY KEY,
                    password TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS messages (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    peer TEXT NOT NULL,
                    direction TEXT NOT NULL,
                    ts INTEGER NOT NULL,
                    payload TEXT NOT NULL,
                    UNIQUE(peer, direction, ts, payload)
                );
                CREATE INDEX IF NOT EXISTS idx_msg_peer_ts ON messages(peer, ts);
                "#,
            )
            .unwrap();
            app.manage(AppState {
                db: Mutex::new(conn),
                ws_tx: Mutex::new(None),
                last_seen: Mutex::new(0),
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            connect,
            send_message,
            set_peer_password,
            get_messages
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
