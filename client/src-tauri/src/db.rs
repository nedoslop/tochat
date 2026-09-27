use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection};
use serde::Serialize;
use tokio::sync::Mutex;

use crate::protocol::StoredMsg;
use crate::util::encode_username;

/// One message as stored locally (frontend representation).
#[derive(Debug, Clone, Serialize)]
pub struct LocalMsg {
    pub id: String,
    pub direction: String, // "in" | "out"
    pub ts: i64,
    pub edit_ts: i64,
    pub kind: String,
    pub payload: String,
}

/// Per-user SQLite database, keyed by the logged-in username.
#[derive(Clone)]
pub struct Database {
    me: String,
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    /// Opens (or creates) `<dir>/<hex(username)>.db`.
    pub fn open(dir: &Path, username: &str) -> rusqlite::Result<Self> {
        let path: PathBuf = dir.join(format!("{}.db", encode_username(username)));
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS messages (
                id        TEXT PRIMARY KEY,
                peer      TEXT NOT NULL,
                direction TEXT NOT NULL,
                ts        INTEGER NOT NULL,
                edit_ts   INTEGER NOT NULL,
                kind      TEXT NOT NULL DEFAULT 'text',
                payload   TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_messages_peer_edit_ts
                ON messages(peer, edit_ts);
            "#,
        )?;
        Ok(Self {
            me: username.to_string(),
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    /// Inserts or replaces a message by id.
    pub async fn upsert(&self, msg: &StoredMsg) {
        let direction = if msg.from == self.me { "out" } else { "in" };
        let peer = if direction == "in" { &msg.from } else { &msg.to };
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO messages (id, peer, direction, ts, edit_ts, kind, payload) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT(id) DO UPDATE SET \
                edit_ts = excluded.edit_ts, \
                kind    = excluded.kind, \
                payload = excluded.payload",
            params![msg.id, peer, direction, msg.ts, msg.edit_ts, msg.kind, msg.payload],
        );
    }

    /// Returns all messages for a peer, ordered by ts.
    pub async fn all_for_peer(&self, peer: &str) -> Vec<LocalMsg> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT id, direction, ts, edit_ts, kind, payload \
             FROM messages WHERE peer = ?1 ORDER BY ts ASC, id ASC",
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
            })
        });
        rows.map(|r| r.filter_map(|x| x.ok()).collect()).unwrap_or_default()
    }

    /// Returns StoredMsg form of all messages with `peer` whose edit_ts > since.
    pub async fn since_for_peer(&self, peer: &str, since: i64) -> Vec<StoredMsg> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT id, direction, ts, edit_ts, kind, payload \
             FROM messages WHERE peer = ?1 AND edit_ts > ?2 ORDER BY edit_ts ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![peer, since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, String>(5)?,
            ))
        });
        let mut out = Vec::new();
        if let Ok(rows) = rows {
            for (id, direction, ts, edit_ts, kind, payload) in rows.flatten() {
                let (from, to) = if direction == "in" {
                    (peer.to_string(), self.me.clone())
                } else {
                    (self.me.clone(), peer.to_string())
                };
                out.push(StoredMsg { id, from, to, ts, edit_ts, kind, payload });
            }
        }
        out
    }

    /// Looks up a locally stored message by id.
    pub async fn get(&self, id: &str) -> Option<StoredMsg> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT peer, direction, ts, edit_ts, kind, payload \
             FROM messages WHERE id = ?1",
            [id],
            |r| {
                let peer: String = r.get(0)?;
                let direction: String = r.get(1)?;
                let ts: i64 = r.get(2)?;
                let edit_ts: i64 = r.get(3)?;
                let kind: String = r.get(4)?;
                let payload: String = r.get(5)?;
                let (from, to) = if direction == "in" {
                    (peer.clone(), self.me.clone())
                } else {
                    (self.me.clone(), peer)
                };
                Ok(StoredMsg { id: id.to_string(), from, to, ts, edit_ts, kind, payload })
            },
        )
        .ok()
    }
}