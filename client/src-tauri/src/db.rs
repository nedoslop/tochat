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
    /// For outgoing: has the peer acknowledged reading it?
    /// For incoming: have we read it (used for unread badges)?
    pub read: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct LocalPeer {
    pub id: i64,
    pub username: String,
    pub display_name: Option<String>,
    pub avatar: Option<String>,
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
    peer_id   INTEGER NOT NULL,
    id        TEXT NOT NULL,
    direction TEXT NOT NULL,
    ts        INTEGER NOT NULL,
    edit_ts   INTEGER NOT NULL,
    kind      TEXT NOT NULL,
    payload   TEXT NOT NULL,
    plaintext INTEGER NOT NULL DEFAULT 0,
    read      INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (peer_id, id)
);

CREATE INDEX IF NOT EXISTS idx_messages_peer_ts ON messages(peer_id, ts);

CREATE TABLE IF NOT EXISTS peers (
    id           INTEGER PRIMARY KEY,
    username     TEXT NOT NULL,
    display_name TEXT,
    avatar       TEXT
);

-- Per-chat encryption. Peer 0 is the Notes chat.
-- `method` is one of "none" | "shared_password" | "pre_shared_key".
-- `secret` is the password / raw hex key. Nothing here ever touches the
-- server — it's a local-only table, one row per chat.
CREATE TABLE IF NOT EXISTS peer_encryption (
    peer_id INTEGER PRIMARY KEY,
    method  TEXT NOT NULL,
    secret  TEXT
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

    // ---- peers ----

    pub async fn upsert_peer(
        &self,
        id: i64,
        username: &str,
        display_name: Option<&str>,
        avatar: Option<&str>,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO peers (id, username, display_name, avatar) \
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(id) DO UPDATE SET
                 username     = excluded.username,
                 display_name = excluded.display_name,
                 avatar       = excluded.avatar",
            params![id, username, display_name, avatar],
        );
    }

    pub async fn get_peer(&self, id: i64) -> Option<LocalPeer> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, username, display_name, avatar FROM peers WHERE id = ?1",
            [id],
            |r| {
                Ok(LocalPeer {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    display_name: r.get(2)?,
                    avatar: r.get(3)?,
                })
            },
        )
        .ok()
    }

    pub async fn peer_id_by_username(&self, username: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id FROM peers WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .ok()
    }

    pub async fn list_peers(&self) -> Vec<LocalPeer> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare("SELECT id, username, display_name, avatar FROM peers") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok(LocalPeer {
                id: r.get(0)?,
                username: r.get(1)?,
                display_name: r.get(2)?,
                avatar: r.get(3)?,
            })
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    // ---- messages ----

    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_message(
        &self,
        peer_id: i64,
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
             (peer_id, id, direction, ts, edit_ts, kind, payload, plaintext, read)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(peer_id, id) DO UPDATE SET
                 ts        = excluded.ts,
                 edit_ts   = excluded.edit_ts,
                 kind      = excluded.kind,
                 payload   = excluded.payload,
                 plaintext = excluded.plaintext,
                 read      = MAX(messages.read, excluded.read)",
            params![
                peer_id,
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

    /// Returns up to `limit` messages strictly older than `before_ts`
    /// (or the newest `limit` if `before_ts` is None), oldest-first.
    ///
    /// IMPORTANT: `limit = None` is bound as `-1` (SQLite's "no limit"),
    /// NOT NULL. `LIMIT NULL` is silently treated as `LIMIT 0` by SQLite.
    pub async fn get_messages(
        &self,
        peer_id: i64,
        before_ts: Option<i64>,
        limit: Option<u32>,
    ) -> Vec<LocalMsg> {
        let conn = self.conn.lock().await;
        let limit_i64: i64 = match limit {
            Some(l) => l as i64,
            None => -1,
        };
        let mut stmt = match conn.prepare(
            "SELECT id, direction, ts, edit_ts, kind, payload, plaintext, read
             FROM messages
             WHERE peer_id = ?1 AND (?2 IS NULL OR ts < ?2)
             ORDER BY ts DESC, edit_ts DESC
             LIMIT ?3",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![peer_id, before_ts, limit_i64], |r| {
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
        let mut msgs: Vec<_> = match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        };
        msgs.reverse();
        msgs
    }

    pub async fn has_messages_before(&self, peer_id: i64, before_ts: i64) -> bool {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT 1 FROM messages WHERE peer_id = ?1 AND ts < ?2 LIMIT 1",
            params![peer_id, before_ts],
            |_| Ok(()),
        )
        .is_ok()
    }

    /// Marks our outgoing messages to `peer_id` (up to `up_to_ts`) as read by peer.
    pub async fn mark_read_up_to(&self, peer_id: i64, up_to_ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE messages SET read = 1
             WHERE peer_id = ?1 AND direction = 'out' AND ts <= ?2 AND read = 0",
            params![peer_id, up_to_ts],
        );
    }

    /// Marks incoming messages from `peer_id` (up to `up_to_ts`) as read by us.
    pub async fn mark_incoming_read(&self, peer_id: i64, up_to_ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE messages SET read = 1
             WHERE peer_id = ?1 AND direction = 'in' AND ts <= ?2 AND read = 0",
            params![peer_id, up_to_ts],
        );
    }

    pub async fn unread_counts(&self) -> Vec<(i64, i64)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT peer_id, COUNT(*) FROM messages
             WHERE direction = 'in' AND read = 0
             GROUP BY peer_id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn clear_peer(&self, peer_id: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages WHERE peer_id = ?1", [peer_id]);
    }

    // ---- per-chat encryption ----

    /// Persists the encryption config for `peer_id`. Method "none" removes
    /// the row (so plaintext chats stay truly stateless).
    pub async fn set_encryption(&self, peer_id: i64, method: &str, secret: Option<&str>) {
        let conn = self.conn.lock().await;
        if method == "none" {
            let _ = conn.execute("DELETE FROM peer_encryption WHERE peer_id = ?1", [peer_id]);
        } else {
            let _ = conn.execute(
                "INSERT INTO peer_encryption (peer_id, method, secret) \
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(peer_id) DO UPDATE SET
                     method = excluded.method,
                     secret = excluded.secret",
                params![peer_id, method, secret],
            );
        }
    }

    pub async fn load_all_encryption(&self) -> Vec<(i64, String, Option<String>)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare("SELECT peer_id, method, secret FROM peer_encryption") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn wipe_all(&self) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages", []);
        let _ = conn.execute("DELETE FROM peers", []);
        let _ = conn.execute("DELETE FROM peer_encryption", []);
        let _ = conn.execute("DELETE FROM settings", []);
    }
}
