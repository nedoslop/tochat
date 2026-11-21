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

/// One row for the batched history upsert (single transaction).
pub struct MsgRow {
    pub peer_id: i64,
    pub id: String,
    pub direction: String,
    pub ts: i64,
    pub edit_ts: i64,
    pub kind: String,
    pub payload: String,
    pub plaintext: bool,
    pub read: bool,
}

const UPSERT_SQL: &str = "INSERT INTO messages
    (peer_id, id, direction, ts, edit_ts, kind, payload, plaintext, read)
    VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
    ON CONFLICT(peer_id, id) DO UPDATE SET
        ts        = excluded.ts,
        edit_ts   = excluded.edit_ts,
        kind      = excluded.kind,
        payload   = excluded.payload,
        plaintext = excluded.plaintext,
        read      = MAX(messages.read, excluded.read)";

#[derive(Clone)]
pub struct Database {
    conn: Arc<Mutex<Connection>>,
}

impl Database {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        // Multiple instances of the client can share the same on-disk DB.
        conn.busy_timeout(std::time::Duration::from_millis(5000))?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            PRAGMA synchronous = NORMAL;

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
            -- Incremental history sync: "everything changed since X".
            CREATE INDEX IF NOT EXISTS idx_messages_peer_edit ON messages(peer_id, edit_ts);
            -- Sync cursor: MAX(edit_ts) over INCOMING messages only.
            CREATE INDEX IF NOT EXISTS idx_messages_peer_dir_edit
                ON messages(peer_id, direction, edit_ts);

            CREATE TABLE IF NOT EXISTS peers (
                id           INTEGER PRIMARY KEY,
                username     TEXT NOT NULL,
                display_name TEXT,
                avatar       TEXT
            );

            -- Per-chat encryption. Peer 0 is the Notes chat. Local-only.
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
        let mut stmt =
            match conn.prepare_cached("SELECT id, username, display_name, avatar FROM peers") {
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
            UPSERT_SQL,
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

    /// Batched upsert in ONE transaction. A 50-message history part used to
    /// be 50 separate commits (50 WAL fsync-ish round-trips under the lock).
    pub async fn upsert_many(&self, rows: &[MsgRow]) {
        if rows.is_empty() {
            return;
        }
        let mut conn = self.conn.lock().await;
        let tx = match conn.transaction() {
            Ok(t) => t,
            Err(_) => return,
        };
        {
            let mut stmt = match tx.prepare_cached(UPSERT_SQL) {
                Ok(s) => s,
                Err(_) => return,
            };
            for r in rows {
                let _ = stmt.execute(params![
                    r.peer_id,
                    r.id,
                    r.direction,
                    r.ts,
                    r.edit_ts,
                    r.kind,
                    r.payload,
                    r.plaintext as i64,
                    r.read as i64
                ]);
            }
        }
        let _ = tx.commit();
    }

    /// Single-message lookup by primary key. Edit/delete used to load the
    /// ENTIRE chat (all media payloads) just to find one row.
    pub async fn get_message(&self, peer_id: i64, id: &str) -> Option<LocalMsg> {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT id, direction, ts, edit_ts, kind, payload, plaintext, read
             FROM messages WHERE peer_id = ?1 AND id = ?2",
            params![peer_id, id],
            |r| {
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
            },
        )
        .ok()
    }

    /// Returns up to `limit` messages strictly older than `before_ts`
    /// (or the newest `limit` if `before_ts` is None), oldest-first.
    ///
    /// `limit = None` is bound as `-1` (SQLite's "no limit"), NOT NULL.
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
        let mut stmt = match conn.prepare_cached(
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

    /// Incremental sync source: messages CHANGED after `since` (optionally
    /// strictly older than `before`), newest-first capped at `limit`,
    /// returned oldest-first. Filtering happens IN SQL via the
    /// (peer_id, edit_ts) index — an up-to-date chat returns instantly
    /// without loading a single payload byte.
    pub async fn get_messages_since(
        &self,
        peer_id: i64,
        since: i64,
        before: Option<i64>,
        limit: Option<u32>,
    ) -> Vec<LocalMsg> {
        let conn = self.conn.lock().await;
        let limit_i64: i64 = match limit {
            Some(l) => l as i64,
            None => -1,
        };
        let mut stmt = match conn.prepare_cached(
            "SELECT id, direction, ts, edit_ts, kind, payload, plaintext, read
             FROM messages
             WHERE peer_id = ?1 AND edit_ts > ?2 AND (?3 IS NULL OR ts < ?3)
             ORDER BY ts DESC, edit_ts DESC
             LIMIT ?4",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        let rows = stmt.query_map(params![peer_id, since, before, limit_i64], |r| {
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

    /// Sync cursor for a peer: newest edit_ts among INCOMING messages.
    ///
    /// Incoming-only is essential: our own outgoing messages written while
    /// the peer was offline must NOT advance the cursor, or we would skip
    /// the peer's older-but-undelivered messages on the next sync.
    pub async fn max_incoming_edit_ts(&self, peer_id: i64) -> i64 {
        let conn = self.conn.lock().await;
        conn.query_row(
            "SELECT COALESCE(MAX(edit_ts), 0) FROM messages
             WHERE peer_id = ?1 AND direction = 'in'",
            [peer_id],
            |r| r.get(0),
        )
        .unwrap_or(0)
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
        let mut stmt = match conn.prepare_cached(
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

    pub async fn delete_peer(&self, peer_id: i64) {
        let conn = self.conn.lock().await;
        let _ = conn.execute("DELETE FROM messages WHERE peer_id = ?1", [peer_id]);
        let _ = conn.execute("DELETE FROM peers WHERE id = ?1", [peer_id]);
        let _ = conn.execute("DELETE FROM peer_encryption WHERE peer_id = ?1", [peer_id]);
    }

    // ---- per-chat encryption ----

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
        let mut stmt =
            match conn.prepare_cached("SELECT peer_id, method, secret FROM peer_encryption") {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_db(name: &str) -> (Database, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "tochat_test_{}_{}_{}.db",
            name,
            std::process::id(),
            crate::util::random_id().replace(['/', '+', '='], "_")
        ));
        (Database::open(&path).expect("open db"), path)
    }

    fn cleanup(path: &std::path::Path) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{}", path.display(), suffix));
        }
    }

    #[tokio::test]
    async fn upsert_edit_and_unlimited_fetch() {
        let (db, path) = temp_db("crud");
        db.upsert_message(1, "m1", "out", 1000, 1000, "text", "Hello", true, false)
            .await;
        db.upsert_message(1, "m2", "in", 2000, 2000, "text", "World", true, false)
            .await;
        db.upsert_message(
            1,
            "m1",
            "out",
            1000,
            1500,
            "text",
            "Hello edited",
            true,
            false,
        )
        .await;

        let all = db.get_messages(1, None, None).await;
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].id, "m1");
        assert_eq!(all[0].payload, "Hello edited");
        assert_eq!(all[0].edit_ts, 1500);
        assert_eq!(all[1].id, "m2");

        let one = db.get_message(1, "m1").await.unwrap();
        assert_eq!(one.payload, "Hello edited");
        assert!(db.get_message(1, "nope").await.is_none());

        drop(db);
        cleanup(&path);
    }

    #[tokio::test]
    async fn pagination_is_oldest_first_and_strictly_before() {
        let (db, path) = temp_db("page");
        for i in 1..=5 {
            db.upsert_message(
                7,
                &format!("m{i}"),
                "in",
                i * 100,
                i * 100,
                "text",
                "x",
                true,
                false,
            )
            .await;
        }
        let newest2 = db.get_messages(7, None, Some(2)).await;
        assert_eq!(
            newest2.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["m4", "m5"]
        );
        let older = db.get_messages(7, Some(400), Some(2)).await;
        assert_eq!(
            older.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["m2", "m3"]
        );
        assert!(db.has_messages_before(7, 200).await);
        assert!(!db.has_messages_before(7, 100).await);
        drop(db);
        cleanup(&path);
    }

    #[tokio::test]
    async fn incremental_sync_cursor_and_since_filter() {
        let (db, path) = temp_db("sync");
        // Peer's messages (incoming) at t=100, 200; our own later reply at 900.
        db.upsert_message(3, "a", "in", 100, 100, "text", "a", true, false)
            .await;
        db.upsert_message(3, "b", "in", 200, 200, "text", "b", true, false)
            .await;
        db.upsert_message(3, "c", "out", 900, 900, "text", "c", true, false)
            .await;

        // Cursor must ignore our own outgoing message (edge case: we wrote
        // while the peer was offline; their older messages must still sync).
        assert_eq!(db.max_incoming_edit_ts(3).await, 200);

        // Nothing changed since the cursor -> empty, instantly.
        assert!(db
            .get_messages_since(3, 200, None, Some(50))
            .await
            .is_empty());

        // An edit of an old message resurfaces it (edit_ts advances).
        db.upsert_message(3, "a", "in", 100, 950, "text", "a!", true, false)
            .await;
        let changed = db.get_messages_since(3, 200, None, Some(50)).await;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].id, "a");
        assert_eq!(changed[0].payload, "a!");

        // `before` pagination still works with since = 0.
        let older = db.get_messages_since(3, 0, Some(900), Some(50)).await;
        assert_eq!(
            older.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"]
        );

        drop(db);
        cleanup(&path);
    }

    #[tokio::test]
    async fn batched_upsert_matches_single() {
        let (db, path) = temp_db("batch");
        let rows: Vec<MsgRow> = (1..=10)
            .map(|i| MsgRow {
                peer_id: 5,
                id: format!("m{i}"),
                direction: "in".into(),
                ts: i * 10,
                edit_ts: i * 10,
                kind: "text".into(),
                payload: "x".into(),
                plaintext: true,
                read: false,
            })
            .collect();
        db.upsert_many(&rows).await;
        assert_eq!(db.get_messages(5, None, None).await.len(), 10);
        // Re-upsert must not regress read flags.
        db.mark_incoming_read(5, 100).await;
        db.upsert_many(&rows).await;
        assert!(db.get_messages(5, None, None).await.iter().all(|m| m.read));
        drop(db);
        cleanup(&path);
    }

    #[tokio::test]
    async fn read_flags_never_regress_and_respect_direction() {
        let (db, path) = temp_db("read");
        db.upsert_message(1, "out1", "out", 100, 100, "text", "a", true, false)
            .await;
        db.upsert_message(1, "in1", "in", 200, 200, "text", "b", true, false)
            .await;
        assert_eq!(db.unread_counts().await, vec![(1, 1)]);
        db.mark_read_up_to(1, 500).await; // only touches outgoing
        let msgs = db.get_messages(1, None, None).await;
        assert!(msgs.iter().find(|m| m.id == "out1").unwrap().read);
        assert!(!msgs.iter().find(|m| m.id == "in1").unwrap().read);
        db.mark_incoming_read(1, 500).await;
        assert!(db.unread_counts().await.is_empty());
        db.upsert_message(1, "in1", "in", 200, 200, "text", "b", true, false)
            .await;
        assert!(db.get_messages(1, None, None).await.iter().all(|m| m.read));
        drop(db);
        cleanup(&path);
    }

    #[tokio::test]
    async fn encryption_rows_and_peer_deletion() {
        let (db, path) = temp_db("enc");
        db.upsert_peer(3, "bob", Some("Bob"), None).await;
        db.upsert_message(3, "m", "in", 1, 1, "text", "x", true, false)
            .await;
        db.set_encryption(3, "pre_shared_key", Some("ab")).await;
        assert_eq!(db.load_all_encryption().await.len(), 1);
        db.set_encryption(3, "none", None).await;
        assert!(db.load_all_encryption().await.is_empty());
        db.set_encryption(3, "shared_password", Some("pw")).await;
        db.delete_peer(3).await;
        assert!(db.get_peer(3).await.is_none());
        assert!(db.get_messages(3, None, None).await.is_empty());
        assert!(db.load_all_encryption().await.is_empty());
        drop(db);
        cleanup(&path);
    }
}
