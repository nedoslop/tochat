use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

/// Thread-safe SQLite wrapper with a serialized connection.
#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

/// Returns (smaller, larger) of two ids for canonical pair ordering.
fn order(a: i64, b: i64) -> (i64, i64) {
    if a < b {
        (a, b)
    } else {
        (b, a)
    }
}

impl Database {
    /// Opens (or creates) the database and initializes the schema.
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS users (
                id            INTEGER PRIMARY KEY AUTOINCREMENT,
                username      TEXT NOT NULL UNIQUE,
                password_hash TEXT NOT NULL,
                last_seen     INTEGER NOT NULL DEFAULT 0
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
            "#,
        )?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    // ---- users -------------------------------------------------------

    /// Returns (user_id, password_hash, last_seen) for a username.
    pub async fn get_user(&self, username: &str) -> Option<(i64, String, i64)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, password_hash, last_seen FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
    }

    /// Returns the user id for a username, if the user exists.
    pub async fn user_id(&self, username: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id FROM users WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .ok()
    }

    /// Inserts a new user; returns the new id, or `None` if the username is taken.
    pub async fn create_user(&self, username: &str, password_hash: &str) -> Option<i64> {
        let conn = self.conn.lock().await;
        let inserted = conn
            .execute(
                "INSERT OR IGNORE INTO users (username, password_hash, last_seen) VALUES (?1, ?2, 0)",
                params![username, password_hash],
            )
            .unwrap_or(0);
        if inserted == 0 {
            return None;
        }
        conn.query_row(
            "SELECT id FROM users WHERE username = ?1",
            [username],
            |r| r.get(0),
        )
        .ok()
    }

    /// Updates the last_seen timestamp for a user id.
    pub async fn update_last_seen(&self, user_id: i64, ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE users SET last_seen = ?1 WHERE id = ?2",
            params![ts, user_id],
        );
    }

    /// Deletes a user and all their relationships.
    pub async fn delete_user(&self, user_id: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "DELETE FROM relationships WHERE user_a = ?1 OR user_b = ?1",
            [user_id],
        );
        let _ = conn.execute("DELETE FROM users WHERE id = ?1", [user_id]);
    }

    // ---- relationships -----------------------------------------------

    /// Ensures a pending relationship exists with `initiator` as the initiator.
    pub async fn ensure_relationship_initiated(&self, initiator: i64, other: i64) {
        let (a, b) = order(initiator, other);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO relationships (user_a, user_b, initiator, established) \
             VALUES (?1, ?2, ?3, 0)",
            params![a, b, initiator],
        );
    }

    /// Returns (initiator_id, established) for a relationship, if any.
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

    /// Marks a relationship as established (both sides accepted).
    pub async fn establish_relationship(&self, u1: i64, u2: i64) {
        let (a, b) = order(u1, u2);
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "UPDATE relationships SET established = 1 WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
        );
    }

    /// Returns usernames of all established peers of a user.
    pub async fn list_peers(&self, user_id: i64) -> Vec<String> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT u.username FROM relationships r \
             JOIN users u ON u.id = CASE WHEN r.user_a = ?1 THEN r.user_b ELSE r.user_a END \
             WHERE (r.user_a = ?1 OR r.user_b = ?1) AND r.established = 1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    /// Returns usernames of pending chats (users who messaged us first).
    pub async fn list_pending_chats(&self, user_id: i64) -> Vec<String> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT u.username FROM relationships r \
             JOIN users u ON u.id = r.initiator \
             WHERE r.established = 0 AND r.initiator != ?1 \
             AND (r.user_a = ?1 OR r.user_b = ?1)",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([user_id], |r| r.get::<_, String>(0));
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }
}