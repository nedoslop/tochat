use serde::{Deserialize, Serialize};

pub const KIND_TEXT: &str = "text";

/// Messages sent by the client to the server.
#[derive(Debug, Clone, Serialize)]
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

/// Messages sent by the server to the client.
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
    Close {
        reason: String,
    },
}
