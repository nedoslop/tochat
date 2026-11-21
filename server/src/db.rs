use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

fn order(a: i64, b: i64) -> (i64, i64) {
    if a < b { (a, b) } else { (b, a) }
}

/// Everything `handle_send` / `handle_edit` / history relay need to know
/// about a (me, peer) pair, fetched under ONE connection lock instead of
/// four separate lock/query round-trips per message.
pub struct SendContext {
    pub exists: bool,
    pub blocked: bool,
    pub relationship: Option<(i64, bool)>,
}

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;
            PRAGMA foreign_keys = ON;
            PRAGMA busy_timeout = 5000;

            CREATE TABLE IF NOT EXISTS users (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                username      TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                last_seen     INTEGER NOT NULL DEFAULT 0,
                display_name  TEXT,
                avatar        TEXT
            );

            CREATE TABLE IF NOT EXISTS relationships (
                user_a      INTEGER NOT NULL,
                user_b      INTEGER NOT NULL,
                initiator   INTEGER NOT NULL,
                established INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_a, user_b),
                FOREIGN KEY (user_a) REFERENCES users(id) ON DELETE CASCADE,
                FOREIGN KEY (user_b) REFERENCES users(id) ON DELETE CASCADE
            );
            CREATE INDEX IF NOT EXISTS idx_rel_user_b ON relationships(user_b);

            CREATE TABLE IF NOT EXISTS blocks (
                blocker INTEGER NOT NULL,
                blocked INTEGER NOT NULL,
                PRIMARY KEY (blocker, blocked),
                FOREIGN KEY (blocker) REFERENCES users(id) ON DELETE CASCADE,
                FOREIGN KEY (blocked) REFERENCES users(id) ON DELETE CASCADE
            );
            "#,
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)) })
    }

    // ---- users ----

    pub async fn get_user(&self, username: &str) -> Option<(i64, String, i64)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, password_hash, last_seen FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    }

    pub async fn get_user_by_id(&self, user_id: i64) -> Option<(String, String, i64)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT username, password_hash, last_seen FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    }

    pub async fn username(&self, user_id: i64) -> Option<String> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT username FROM users WHERE id = ?1", [user_id], |r| r.get(0))
            .ok()
    }

    /// Batch username resolution: ONE lock + a cached statement for all ids,
    /// instead of N async lock round-trips (handshake / peer lists).
    pub async fn peer_infos(&self, ids: &[i64]) -> Vec<(i64, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare_cached("SELECT username FROM users WHERE id = ?1") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            if let Ok(name) = stmt.query_row([id], |r| r.get::<_, String>(0)) {
                out.push((id, name));
            }
        }
        out
    }

    pub async fn get_profile(&self, user_id: i64) -> Option<(Option<String>, Option<String>)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT display_name, avatar FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    }

    pub async fn get_user_profile(&self, user_id: i64) -> Option<(String, Option<String>, Option<String>)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT username, display_name, avatar FROM users WHERE id = ?1",
            [user_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    }

    /// Batch profile resolution (handshake ships every contact's profile).
    pub async fn profiles(&self, ids: &[i64]) -> Vec<(i64, String, Option<String>, Option<String>)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn
            .prepare_cached("SELECT username, display_name, avatar FROM users WHERE id = ?1")
        {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let mut out = Vec::with_capacity(ids.len());
        for &id in ids {
            if let Ok(row) = stmt.query_row([id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?, r.get::<_, Option<String>>(2)?))
            }) {
                out.push((id, row.0, row.1, row.2));
            }
        }
        out
    }

    pub async fn set_profile(
        &self,
        user_id: i64,
        display_name: Option<&str>,
        avatar: Option<&str>,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE users SET display_name = ?1, avatar = ?2 WHERE id = ?3",
            params![display_name, avatar, user_id],
        );
    }

    pub async fn user_id(&self, username: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        conn.query_row("SELECT id FROM users WHERE username = ?1", [username], |r| r.get(0))
            .ok()
    }

    pub async fn create_user(&self, username: &str, password_hash: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        let mut stmt = conn
            .prepare_cached(
                "INSERT OR IGNORE INTO users (username, password_hash, last_seen) \
                 VALUES (?1, ?2, 0) RETURNING id",
            )
            .ok()?;
        stmt.query_row(params![username, password_hash], |r| r.get(0)).ok()
    }

    pub async fn update_last_seen(&self, user_id: i64, ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE users SET last_seen = ?1 WHERE id = ?2",
            params![ts, user_id],
        );
    }

    pub async fn delete_user(&self, user_id: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM relationships WHERE user_a = ?1 OR user_b = ?1", [user_id]);
        let _ = conn.execute("DELETE FROM blocks WHERE blocker = ?1 OR blocked = ?1", [user_id]);
        let _ = conn.execute("DELETE FROM users WHERE id = ?1", [user_id]);
    }

    // ---- relationships ----

    pub async fn ensure_relationship_initiated(&self, initiator: i64, other: i64) {
        let (a, b) = order(initiator, other);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO relationships (user_a, user_b, initiator, established) \
             VALUES (?1, ?2, ?3, 0)",
            params![a, b, initiator],
        );
    }

    pub async fn get_relationship(&self, u1: i64, u2: i64) -> Option<(i64, bool)> {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT initiator, established FROM relationships WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .ok()
    }

    pub async fn establish_relationship(&self, u1: i64, u2: i64) {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE relationships SET established = 1 WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
        );
    }

    pub async fn delete_relationship(&self, u1: i64, u2: i64) {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "DELETE FROM relationships WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
        );
    }

    pub async fn list_all_relationships(&self, user_id: i64) -> Vec<i64> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare_cached(
            "SELECT CASE WHEN user_a = ?1 THEN user_b ELSE user_a END \
             FROM relationships \
             WHERE user_a = ?1 OR user_b = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, i64>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn list_peers(&self, user_id: i64) -> Vec<i64> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare_cached(
            "SELECT CASE WHEN user_a = ?1 THEN user_b ELSE user_a END \
             FROM relationships \
             WHERE (user_a = ?1 OR user_b = ?1) AND established = 1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, i64>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn list_pending_chats(&self, user_id: i64) -> Vec<i64> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare_cached(
            "SELECT initiator FROM relationships \
             WHERE established = 0 AND initiator != ?1 \
             AND (user_a = ?1 OR user_b = ?1)",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, i64>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    // ---- blocks ----

    pub async fn add_block(&self, blocker: i64, blocked: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO blocks (blocker, blocked) VALUES (?1, ?2)",
            params![blocker, blocked],
        );
    }

    pub async fn remove_block(&self, blocker: i64, blocked: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "DELETE FROM blocks WHERE blocker = ?1 AND blocked = ?2",
            params![blocker, blocked],
        );
    }

    pub async fn is_blocked(&self, blocker: i64, blocked: i64) -> bool {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT 1 FROM blocks WHERE blocker = ?1 AND blocked = ?2",
            params![blocker, blocked],
            |_| Ok(()),
        )
        .is_ok()
    }

    pub async fn list_blocks(&self, user_id: i64) -> Vec<i64> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare_cached("SELECT blocked FROM blocks WHERE blocker = ?1") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, i64>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    // ---- combined hot-path checks ----

    /// One lock, three cached queries: target existence, a block in EITHER
    /// direction, and the relationship row. Used per message send/edit and
    /// per history part, where latency matters.
    pub async fn send_context(&self, me: i64, to: i64) -> SendContext {
        let (a, b) = order(me, to);
        let conn = self.conn.lock().await;

        let exists = conn
            .query_row("SELECT 1 FROM users WHERE id = ?1", [to], |_| Ok(()))
            .is_ok();

        let blocked = conn
            .query_row(
                "SELECT 1 FROM blocks \
                 WHERE (blocker = ?1 AND blocked = ?2) OR (blocker = ?2 AND blocked = ?1)",
                params![me, to],
                |_| Ok(()),
            )
            .is_ok();

        let relationship = conn
            .query_row(
                "SELECT initiator, established FROM relationships WHERE user_a = ?1 AND user_b = ?2",
                params![a, b],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)? != 0)),
            )
            .ok();

        SendContext { exists, blocked, relationship }
    }
}