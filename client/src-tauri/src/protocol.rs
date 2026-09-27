use serde::{Deserialize, Serialize};

/// Message kind constant (only text for now).
pub const KIND_TEXT: &str = "text";

/// Client -> server messages.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: String,
        id: String,
        ts: i64,
        kind: String,
        payload: String,
    },
    Edit {
        to: String,
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: String,
        payload: String,
    },
    PullHistory {
        from: String,
        since: i64,
    },
    HistoryResponse {
        to: String,
        messages: Vec<StoredMsg>,
    },
    ListPending,
    DeleteAccount {
        password: String,
    },
}

/// A message as it travels over the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMsg {
    pub id: String,
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub edit_ts: i64,
    #[serde(default = "default_kind")]
    pub kind: String,
    pub payload: String,
}

fn default_kind() -> String {
    KIND_TEXT.to_string()
}

/// Server -> client messages.
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk {
        username: String,
        last_seen: i64,
    },
    Peers {
        peers: Vec<String>,
    },
    PendingChats {
        users: Vec<String>,
    },
    PeerOnline {
        username: String,
    },
    PeerOffline {
        username: String,
    },
    Message {
        id: String,
        from: String,
        ts: i64,
        edit_ts: i64,
        kind: String,
        payload: String,
    },
    PullHistoryRequest {
        from: String,
        since: i64,
    },
    HistoryResponse {
        from: String,
        messages: Vec<StoredMsg>,
    },
    Error {
        msg: String,
    },
    /// Server-initiated close. `reason` is "session_taken_over" or "account_deleted".
    Close {
        reason: String,
    },
}