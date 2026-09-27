use serde::{Deserialize, Serialize};

/// Message kind constant (only text for now).
pub const KIND_TEXT: &str = "text";

/// Messages sent by a client to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    /// Send a new message to a peer.
    Send {
        to: String,
        id: String,
        ts: i64,
        kind: String,
        payload: String,
    },
    /// Edit (empty payload = delete) an existing message.
    Edit {
        to: String,
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: String,
        payload: String,
    },
    /// Request history from a peer since a given edit_ts (exclusive).
    PullHistory { from: String, since: i64 },
    /// Reply to a PullHistoryRequest.
    HistoryResponse {
        to: String,
        messages: Vec<StoredMsg>,
    },
    /// Request the current pending (unaccepted) chat list.
    ListPending,
    /// Delete the caller's account.
    DeleteAccount { password: String },
}

/// A stored message as relayed by the server.
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

/// Messages sent by the server to a client.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk { username: String, last_seen: i64 },
    Peers { peers: Vec<String> },
    PendingChats { users: Vec<String> },
    PeerOnline { username: String },
    PeerOffline { username: String },
    /// A new message or an in-place update to an existing one (id-keyed).
    Message {
        id: String,
        from: String,
        ts: i64,
        edit_ts: i64,
        kind: String,
        payload: String,
    },
    PullHistoryRequest { from: String, since: i64 },
    HistoryResponse { from: String, messages: Vec<StoredMsg> },
    Error { msg: String },
    Close,
}