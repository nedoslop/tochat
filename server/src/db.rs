use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

/// Thin async wrapper around a single SQLite connection.
///
/// All queries go through this type so the rest of the codebase does not
/// depend on `rusqlite` at all.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

fn order<'a>(a: &'a str, b: &'a str) -> (&'a str, &'a str) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;

            CREATE TABLE IF NOT EXISTS users (
                username      TEXT PRIMARY KEY,
                password_hash TEXT NOT NULL,
                last_seen     INTEGER NOT NULL DEFAULT 0
            );

            -- One row per (unordered) pair of users.
            --
            -- initiator    = username that sent the *first* message.
            -- established  = 0 -> only the initiator knows the chat exists;
            --                    the other side must pull history first.
            --              = 1 -> both sides may freely chat.
            CREATE TABLE IF NOT EXISTS relationships (
                user_a       TEXT NOT NULL,
                user_b       TEXT NOT NULL,
                initiator    TEXT NOT NULL,
                established  INTEGER NOT NULL DEFAULT 0,
                PRIMARY KEY (user_a, user_b)
            );
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ---- users --------------------------------------------------------

    pub async fn get_user(&self, username: &str) -> Option<(String, i64)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT password_hash, last_seen FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .ok()
    }

    /// Returns true if the account was created, false if it already existed.
    pub async fn create_user(&self, username: &str, password_hash: &str) -> bool {
        let conn = self.conn.lock().await;
        conn.execute(
            "INSERT OR IGNORE INTO users (username, password_hash, last_seen) \
             VALUES (?1, ?2, 0)",
            params![username, password_hash],
        )
        .map(|n| n > 0)
        .unwrap_or(false)
    }

    pub async fn update_last_seen(&self, username: &str, ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE users SET last_seen = ?1 WHERE username = ?2",
            params![ts, username],
        );
    }

    pub async fn delete_user(&self, username: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM users WHERE username = ?1", [username]);
        let _ = conn.execute(
            "DELETE FROM relationships WHERE user_a = ?1 OR user_b = ?1",
            [username],
        );
    }

    // ---- relationships ------------------------------------------------

    /// Ensure a relationship row exists with `initiator` as the first-message
    /// sender. Does nothing if a row already exists.
    pub async fn ensure_relationship_initiated(&self, initiator: &str, other: &str) {
        let (a, b) = order(initiator, other);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO relationships \
             (user_a, user_b, initiator, established) VALUES (?1, ?2, ?3, 0)",
            params![a, b, initiator],
        );
    }

    /// Returns `(initiator, established)` for the pair, if it exists.
    pub async fn get_relationship(&self, u1: &str, u2: &str) -> Option<(String, bool)> {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT initiator, established FROM relationships \
             WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)),
        )
        .ok()
    }

    pub async fn establish_relationship(&self, u1: &str, u2: &str) {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE relationships SET established = 1 \
             WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
        );
    }

    /// Established chat partners of `username`.
    pub async fn list_peers(&self, username: &str) -> Vec<String> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT CASE WHEN user_a = ?1 THEN user_b ELSE user_a END \
             FROM relationships \
             WHERE (user_a = ?1 OR user_b = ?1) AND established = 1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([username], |r| r.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Users that initiated a chat with `username` but whose pending first
    /// message has not been consumed yet.
    pub async fn list_pending_chats(&self, username: &str) -> Vec<String> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT initiator FROM relationships \
             WHERE established = 0 \
               AND initiator != ?1 \
               AND (user_a = ?1 OR user_b = ?1)",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([username], |r| r.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }
}
