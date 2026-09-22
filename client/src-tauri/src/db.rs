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

fn column_exists(conn: &Connection, table: &str, column: &str) -> bool {
    let sql = format!("PRAGMA table_info({})", table);
    let mut stmt = match conn.prepare(&sql) {
        Ok(s) => s,
        Err(_) => return false,
    };
    let rows = stmt.query_map([], |r| r.get::<_, String>(1));
    match rows {
        Ok(rows) => rows.filter_map(|r| r.ok()).any(|name| name == column),
        Err(_) => false,
    }
}

fn table_exists(conn: &Connection, table: &str) -> bool {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        [table],
        |_| Ok(()),
    )
    .is_ok()
}

impl Database {
    pub fn open(path: &str) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch("PRAGMA journal_mode = WAL;")?;

        // Legacy schema (before owner-scoping) has a `messages` table without
        // an `owner` column, and `peer_encryption` with a single-column PK.
        // We drop and recreate to migrate; local chat history is per-machine
        // cache anyway, and previously it was cross-account mixed up so it
        // wasn't trustworthy across accounts.
        let legacy_messages = table_exists(&conn, "messages")
            && !column_exists(&conn, "messages", "owner");
        let legacy_enc = table_exists(&conn, "peer_encryption")
            && !column_exists(&conn, "peer_encryption", "owner");
        if legacy_messages {
            conn.execute_batch("DROP TABLE IF EXISTS messages;")?;
        }
        if legacy_enc {
            conn.execute_batch("DROP TABLE IF EXISTS peer_encryption;")?;
        }

        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS messages (
                id        INTEGER PRIMARY KEY AUTOINCREMENT,
                owner     TEXT NOT NULL,
                peer      TEXT NOT NULL,
                direction TEXT NOT NULL,
                ts        INTEGER NOT NULL,
                text      TEXT NOT NULL
            );
            -- Uniqueness guard so the same message can never be inserted twice
            -- (dedup across restarts, concurrent pulls, retries, etc.).
            CREATE UNIQUE INDEX IF NOT EXISTS idx_messages_unique
                ON messages(owner, peer, direction, ts, text);
            CREATE INDEX IF NOT EXISTS idx_messages_owner_peer_ts
                ON messages(owner, peer, ts);

            CREATE TABLE IF NOT EXISTS peer_encryption (
                owner    TEXT NOT NULL,
                peer     TEXT NOT NULL,
                method   TEXT NOT NULL,
                password TEXT,
                PRIMARY KEY (owner, peer)
            );
            "#,
        )?;

        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
        })
    }

    pub async fn insert_message(
        &self,
        owner: &str,
        peer: &str,
        direction: &str,
        ts: i64,
        text: &str,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR IGNORE INTO messages (owner, peer, direction, ts, text) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![owner, peer, direction, ts, text],
        );
    }

    pub async fn get_messages(&self, owner: &str, peer: &str) -> Vec<Message> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT direction, ts, text FROM messages \
             WHERE owner = ?1 AND peer = ?2 ORDER BY ts ASC, id ASC",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![owner, peer], |r| {
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

    pub async fn set_peer_encryption(
        &self,
        owner: &str,
        peer: &str,
        method: &str,
        password: Option<&str>,
    ) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO peer_encryption (owner, peer, method, password) \
             VALUES (?1, ?2, ?3, ?4)",
            params![owner, peer, method, password],
        );
    }

    pub async fn get_peer_encryption(&self, owner: &str, peer: &str) -> (String, Option<String>) {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT method, password FROM peer_encryption WHERE owner = ?1 AND peer = ?2",
            params![owner, peer],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap_or(("none".to_string(), None))
    }

    /// Wipe only the current account's local data. Other accounts
    /// (which may be running in parallel app instances) are left alone.
    pub async fn wipe_owner(&self, owner: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages WHERE owner = ?1", [owner]);
        let _ = conn.execute("DELETE FROM peer_encryption WHERE owner = ?1", [owner]);
    }
}