use std::path::Path;
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

use crate::protocol::StoredMsg;

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS peer_settings (
                peer     TEXT PRIMARY KEY,
                password TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS messages (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                peer      TEXT NOT NULL,
                direction TEXT NOT NULL,       -- 'in' | 'out'
                ts        INTEGER NOT NULL,
                payload   TEXT NOT NULL,
                UNIQUE(peer, direction, ts, payload)
            );

            CREATE INDEX IF NOT EXISTS idx_msg_peer_ts ON messages(peer, ts);
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ---- peer shared passwords ---------------------------------------

    pub async fn get_peer_password(&self, peer: &str) -> Option<String> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT password FROM peer_settings WHERE peer = ?1",
            [peer],
            |r| r.get::<_, String>(0),
        )
        .ok()
    }

    pub async fn set_peer_password(&self, peer: &str, password: &str) -> rusqlite::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT INTO peer_settings (peer, password) VALUES (?1, ?2) \
             ON CONFLICT(peer) DO UPDATE SET password = excluded.password",
            params![peer, password],
        )?;
        Ok(())
    }

    // ---- messages -----------------------------------------------------

    /// Insert a message. Returns `true` if it was actually new.
    pub async fn store_message(
        &self,
        peer: &str,
        direction: &str,
        ts: i64,
        payload: &str,
    ) -> rusqlite::Result<bool> {
        let conn = self.conn.lock().await;
        let n = conn.execute(
            "INSERT OR IGNORE INTO messages (peer, direction, ts, payload) \
             VALUES (?1, ?2, ?3, ?4)",
            params![peer, direction, ts, payload],
        )?;
        Ok(n > 0)
    }

    /// Raw rows for a peer, newest-last. Returns `(direction, ts, payload)`.
    pub async fn messages_for_peer(
        &self,
        peer: &str,
    ) -> rusqlite::Result<Vec<(String, i64, String)>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT direction, ts, payload FROM messages \
             WHERE peer = ?1 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map([peer], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        rows.collect()
    }

    /// Everything between `me` and `peer` after `since`, in wire format.
    /// Used to answer a `PullHistoryRequest` coming from `peer`.
    pub async fn history_for_peer(
        &self,
        peer: &str,
        me: &str,
        since: i64,
    ) -> rusqlite::Result<Vec<StoredMsg>> {
        let conn = self.conn.lock().await;
        let mut stmt = conn.prepare(
            "SELECT direction, ts, payload FROM messages \
             WHERE peer = ?1 AND ts > ?2 ORDER BY ts ASC",
        )?;
        let rows = stmt.query_map(params![peer, since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;

        let mut out = Vec::new();
        for row in rows {
            let (direction, ts, payload) = row?;
            let (from, to) = if direction == "out" {
                (me.to_string(), peer.to_string())
            } else {
                (peer.to_string(), me.to_string())
            };
            out.push(StoredMsg {
                from,
                to,
                ts,
                payload,
            });
        }
        Ok(out)
    }

    /// Full wipe — used after the server confirms account deletion.
    pub async fn wipe(&self) -> rusqlite::Result<()> {
        let conn = self.conn.lock().await;
        conn.execute("DELETE FROM messages", [])?;
        conn.execute("DELETE FROM peer_settings", [])?;
        Ok(())
    }
}