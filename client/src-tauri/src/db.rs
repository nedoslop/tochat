use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::{params, Connection};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct LocalDb {
    conn: Arc<Mutex<Connection>>,
    path: PathBuf,
}

impl LocalDb {
    pub fn open(dir: &Path, username: &str) -> rusqlite::Result<Self> {
        std::fs::create_dir_all(dir).ok();
        let path = dir.join(format!("{}.db", sanitize(username)));
        let conn = Connection::open(&path)?;
        conn.execute_batch(
            r#"
PRAGMA journal_mode = WAL;

CREATE TABLE IF NOT EXISTS messages (
    id        INTEGER PRIMARY KEY AUTOINCREMENT,
    peer      TEXT NOT NULL,
    direction TEXT NOT NULL,
    ts        INTEGER NOT NULL,
    text      TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_messages_peer ON messages(peer, ts);

CREATE TABLE IF NOT EXISTS peer_enc (
    peer     TEXT PRIMARY KEY,
    method   TEXT NOT NULL,
    password TEXT
);
"#,
        )?;
        Ok(Self { conn: Arc::new(Mutex::new(conn)), path })
    }

    pub fn path(&self) -> &Path { &self.path }

    pub async fn insert_message(&self, peer: &str, direction: &str, ts: i64, text: &str) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT INTO messages (peer, direction, ts, text) VALUES (?1,?2,?3,?4)",
            params![peer, direction, ts, text],
        );
    }

    pub async fn insert_message_dedup(
        &self,
        peer: &str,
        direction: &str,
        ts: i64,
        text: &str,
    ) -> bool {
        let conn = self.conn.lock().await;
        let exists: bool = conn
            .query_row(
                "SELECT 1 FROM messages WHERE peer=?1 AND direction=?2 AND ts=?3 AND text=?4 LIMIT 1",
                params![peer, direction, ts, text],
                |_| Ok(()),
            )
            .is_ok();
        if exists {
            return false;
        }
        let _ = conn.execute(
            "INSERT INTO messages (peer, direction, ts, text) VALUES (?1,?2,?3,?4)",
            params![peer, direction, ts, text],
        );
        true
    }

    pub async fn get_messages(&self, peer: &str) -> Vec<(String, i64, String)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare(
            "SELECT direction, ts, text FROM messages WHERE peer=?1 ORDER BY ts, id",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([peer], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn set_peer_enc(&self, peer: &str, method: &str, password: Option<&str>) {
        let conn = self.conn.lock().await;
        let _ = conn.execute(
            "INSERT OR REPLACE INTO peer_enc (peer, method, password) VALUES (?1,?2,?3)",
            params![peer, method, password],
        );
    }

    pub async fn get_peer_enc(&self, peer: &str) -> (String, Option<String>) {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT method, password FROM peer_enc WHERE peer=?1",
            [peer],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?)),
        )
        .unwrap_or(("none".into(), None))
    }

    pub async fn all_peer_enc(&self) -> Vec<(String, String, Option<String>)> {
        let conn = self.conn.lock().await;
        let mut stmt = match conn.prepare("SELECT peer, method, password FROM peer_enc") {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        });
        match rows {
            Ok(rows) => rows.filter_map(|r| r.ok()).collect(),
            Err(_) => Vec::new(),
        }
    }

    pub async fn wipe(&self) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages", []);
        let _ = conn.execute("DELETE FROM peer_enc", []);
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}