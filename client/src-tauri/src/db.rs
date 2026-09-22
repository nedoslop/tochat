use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub direction: String,
    pub ts: i64,
    pub text: String,
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS messages (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                peer TEXT NOT NULL,
                direction TEXT NOT NULL,
                ts INTEGER NOT NULL,
                text TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer, ts);
            CREATE TABLE IF NOT EXISTS peer_encryption (
                peer TEXT PRIMARY KEY,
                method TEXT NOT NULL,
                password TEXT
            );
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert_message(&self, peer: &str, direction: &str, ts: i64, text: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO messages (peer, direction, ts, text) VALUES (?1, ?2, ?3, ?4)",
            params![peer, direction, ts, text],
        );
    }

    pub async fn get_messages(&self, peer: &str) -> Vec<Message> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT direction, ts, text FROM messages WHERE peer = ?1 ORDER BY ts ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([peer], |r| {
            Ok(Message {
                direction: r.get(0)?,
                ts: r.get(1)?,
                text: r.get(2)?,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn get_last_ts(&self, peer: &str) -> i64 {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COALESCE(MAX(ts), 0) FROM messages WHERE peer = ?1",
            [peer],
            |r| r.get(0),
        )
        .unwrap_or(0)
    }

    pub async fn set_peer_encryption(&self, peer: &str, method: &str, password: Option<&str>) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO peer_encryption (peer, method, password) VALUES (?1, ?2, ?3)",
            params![peer, method, password],
        );
    }

    pub async fn get_peer_encryption(&self, peer: &str) -> (String, Option<String>) {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT method, password FROM peer_encryption WHERE peer = ?1",
            [peer],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or(("none".to_string(), None))
    }

    pub async fn wipe(&self) {
        let conn = self.conn.lock().await;
        let _ = conn.execute_batch("DELETE FROM messages; DELETE FROM peer_encryption;");
    }
}