use serde::{Deserialize, Serialize};

/// Kind of message payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Text,
    Image,
    Audio,
    File,
}

/// User visibility status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    #[default]
    Online,
    Away,
    Busy,
    Invisible,
}

/// Messages sent by a client to the server.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: String,
        id: String,
        ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    Edit {
        to: String,
        id: String,
        ts: i64,
        edit_ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    /// Send a message to all OTHER sessions belonging to the same user.
    /// Used for syncing the "__notes__" chat across devices.
    SendToSelf {
        id: String,
        ts: i64,
        edit_ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    /// Ask the peer for messages. `since` is a lower bound on edit_ts,
    /// `before` is an upper bound on ts, `limit` caps to the most recent N.
    PullHistory {
        from: String,
        since: i64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        before: Option<i64>,
    },
    HistoryResponse {
        to: String,
        messages: Vec<StoredMsg>,
    },
    ListPending,
    DeleteAccount {
        password: String,
    },
    SetStatus {
        status: UserStatus,
    },
    LeaveChat {
        peer: String,
    },
    BlockUser {
        username: String,
    },
    UnblockUser {
        username: String,
    },
    ListBlocked,
    /// Tell the peer that we've read all messages they sent us up to `up_to_ts`.
    ReadReceipt {
        to: String,
        up_to_ts: i64,
    },
}

/// A stored message as relayed by the server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMsg {
    pub id: String,
    pub from: String,
    pub to: String,
    pub ts: i64,
    pub edit_ts: i64,
    #[serde(default)]
    pub kind: MessageKind,
    pub payload: String,
}

/// Messages sent by the server to a client.
#[derive(Debug, Clone, Serialize)]
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
    StatusUpdate {
        username: String,
        status: UserStatus,
    },
    Message {
        id: String,
        from: String,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    /// A `__notes__` payload from one of our own other devices.
    NoteMessage {
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    PullHistoryRequest {
        from: String,
        since: i64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        before: Option<i64>,
    },
    HistoryResponse {
        from: String,
        messages: Vec<StoredMsg>,
    },
    ReadReceipt {
        from: String,
        up_to_ts: i64,
    },
    Error {
        msg: String,
    },
    Close {
        reason: String,
    },
    ChatLeft {
        peer: String,
    },
    Blocked {
        users: Vec<String>,
    },
}