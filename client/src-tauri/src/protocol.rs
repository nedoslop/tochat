use serde::{Deserialize, Serialize};

/// Messages we send to the server.
#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: String,
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

/// Messages the server pushes to us.
#[derive(Debug, Deserialize)]
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
        from: String,
        payload: String,
        ts: i64,
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
    Close,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMsg {
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub payload: String,
}

/// What the UI receives.
#[derive(Debug, Clone, Serialize)]
pub struct UiMsg {
    pub peer: String,
    /// "in" | "out"
    pub direction: String,
    pub ts: i64,
    pub text: String,
}
