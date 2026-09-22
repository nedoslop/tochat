use serde::{Deserialize, Serialize};

/// Messages a client sends to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Send a chat message to another user.
    Send { to: String, payload: String },

    /// Ask the server to forward a history-pull request to `from`.
    /// Only allowed if `from` has already initiated a chat with us.
    PullHistory { from: String, since: i64 },

    /// Answer to a forwarded history-pull request (we are the initiator).
    HistoryResponse { to: String, messages: Vec<StoredMsg> },

    /// Ask the server for the list of users who initiated a chat with us
    /// and whose pending first message we have not consumed yet.
    ListPending,

    /// Delete our own account (must send current password).
    DeleteAccount { password: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMsg {
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub payload: String,
}

/// Messages the server pushes to a client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk {
        username: String,
        last_seen: i64,
    },
    /// Established chat partners (relationship.established = 1).
    Peers {
        peers: Vec<String>,
    },
    /// Users who have sent us a first message we have not pulled yet.
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
    /// "User `from` wants to pull history from you."
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
    /// Server-initiated close (e.g. after account deletion).
    Close,
}