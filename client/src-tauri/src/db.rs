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
    /// For outgoing messages: has the peer acknowledged reading it?
    pub read: bool,
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
    read      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (peer, id)
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
"#,
        )?;

        // Best-effort migration for pre-existing DBs.
        let _ = conn.execute(
            "ALTER TABLE messages ADD COLUMN read INTEGER NOT NULL DEFAULT 0",
            [],
        );

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
        read: bool,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO messages
                (peer, id, direction, ts, edit_ts, kind, payload, plaintext, read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(peer, id) DO UPDATE SET
                ts        = excluded.ts,
                edit_ts   = excluded.edit_ts,
                kind      = excluded.kind,
                payload   = excluded.payload,
                plaintext = excluded.plaintext,
                read      = MAX(messages.read, excluded.read)",
            params![
                peer,
                id,
                direction,
                ts,
                edit_ts,
                kind,
                payload,
                plaintext as i64,
                read as i64
            ],
        );
    }

    pub async fn get_messages(&self, peer: &str) -> Vec<LocalMsg> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT id, direction, ts, edit_ts, kind, payload, plaintext, read
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
                read: r.get::<_, i64>(7)? != 0,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Marks all outgoing messages to `peer` with `ts <= up_to_ts` as read.
    pub async fn mark_read_up_to(&self, peer: &str, up_to_ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE messages SET read = 1
             WHERE peer = ?1 AND direction = 'out' AND ts <= ?2 AND read = 0",
            params![peer, up_to_ts],
        );
    }

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