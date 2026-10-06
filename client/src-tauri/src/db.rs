use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use serde::Serialize;
use tokio::sync::Mutex;

#[derive(Debug, Clone, Serialize)]
pub struct LocalMsg {
    pub id: String,
    pub direction: String,
    pub ts: i64,
    pub edit_ts: i64,
    pub kind: String,
    pub payload: String,
    pub plaintext: bool,
}

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS messages (
                peer      TEXT NOT NULL,
                id        TEXT NOT NULL,
                direction TEXT NOT NULL,
                ts        INTEGER NOT NULL,
                edit_ts   INTEGER NOT NULL,
                kind      TEXT NOT NULL,
                payload   TEXT NOT NULL,
                plaintext INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (peer, id)
            );

            CREATE TABLE IF NOT EXISTS settings (
                key   TEXT PRIMARY KEY,
                value TEXT NOT NULL
            );
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_message(
        &self,
        peer: &str,
        id: &str,
        direction: &str,
        ts: i64,
        edit_ts: i64,
        kind: &str,
        payload: &str,
        plaintext: bool,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO messages
             (peer, id, direction, ts, edit_ts, kind, payload, plaintext)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT(peer, id) DO UPDATE SET
                ts        = excluded.ts,
                edit_ts   = excluded.edit_ts,
                kind      = excluded.kind,
                payload   = excluded.payload,
                plaintext = excluded.plaintext",
            params![
                peer,
                id,
                direction,
                ts,
                edit_ts,
                kind,
                payload,
                plaintext as i64
            ],
        );
    }

    pub async fn get_messages(&self, peer: &str) -> Vec<LocalMsg> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT id, direction, ts, edit_ts, kind, payload, plaintext
             FROM messages WHERE peer = ?1 ORDER BY ts ASC, edit_ts ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([peer], |r| {
            Ok(LocalMsg {
                id: r.get(0)?,
                direction: r.get(1)?,
                ts: r.get(2)?,
                edit_ts: r.get(3)?,
                kind: r.get(4)?,
                payload: r.get(5)?,
                plaintext: r.get::<_, i64>(6)? != 0,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Removes all messages with a given peer.
    pub async fn clear_peer(&self, peer: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages WHERE peer = ?1", [peer]);
    }

    pub async fn wipe_all(&self) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages", []);
        let _ = conn.execute("DELETE FROM settings", []);
    }
}