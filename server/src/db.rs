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

impl Database {
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
        // Best-effort migration for pre-existing DBs.
        let _ = conn.execute("ALTER TABLE users ADD COLUMN display_name TEXT", []);
        let _ = conn.execute("ALTER TABLE users ADD COLUMN avatar TEXT", []);
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

    pub async fn get_profile(
        &self,
        username: &str,
    ) -> Option<(i64, Option<String>, Option<String>)> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, display_name, avatar FROM users WHERE username = ?1",
            [username],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .ok()
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
            .prepare(
                "INSERT OR IGNORE INTO users (username, password_hash, last_seen) \
                 VALUES (?1, ?2, 0) RETURNING id",
            )
            .ok()?;
        stmt.query_row(params![username, password_hash], |r| r.get(0)).ok()
    }

    pub async fn update_last_seen(&self, user_id: i64, ts: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("UPDATE users SET last_seen = ?1 WHERE id = ?2", params![ts, user_id]);
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

    // ---- blocks ----

    pub async fn add_block(&self, blocker: i64, blocked: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO blocks (blocker, blocked) VALUES (?1, ?2)",
            params![blocker, blocked],
        );
        let (a, b) = order(blocker, blocked);
        let _ = conn.execute(
            "DELETE FROM relationships WHERE user_a = ?1 AND user_b = ?2",
            params![a, b],
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

    pub async fn list_blocks(&self, user_id: i64) -> Vec<String> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT u.username FROM blocks b JOIN users u ON u.id = b.blocked WHERE b.blocker = ?1",
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
