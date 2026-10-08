use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Text,
    Image,
    Audio,
    File,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum UserStatus {
    #[default]
    Online,
    Away,
    Busy,
    Invisible,
}

/// A peer as sent to a client: stable ID + current username.
/// `username` never changes, but is included so the client can render it
/// without a second lookup.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: i64,
        id: String,
        ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    Edit {
        to: i64,
        id: String,
        ts: i64,
        edit_ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    SendToSelf {
        id: String,
        ts: i64,
        edit_ts: i64,
        #[serde(default)]
        kind: MessageKind,
        payload: String,
    },
    PullHistory {
        from: i64,
        since: i64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        before: Option<i64>,
    },
    HistoryResponse {
        to: i64,
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
        peer: i64,
    },
    BlockUser {
        user_id: i64,
    },
    UnblockUser {
        user_id: i64,
    },
    ListBlocked,
    ReadReceipt {
        to: i64,
        up_to_ts: i64,
    },
    /// Resolve a username to a user id (plus profile info). Used when the
    /// user types a new peer name into the "start chat" box.
    GetProfile {
        username: String,
    },
    SetProfile {
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        avatar: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredMsg {
    pub id: String,
    pub from: i64,
    pub to: i64,
    pub ts: i64,
    pub edit_ts: i64,
    #[serde(default)]
    pub kind: MessageKind,
    pub payload: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMsg {
    AuthOk {
        user_id: i64,
        username: String,
        last_seen: i64,
    },
    Peers {
        peers: Vec<PeerInfo>,
    },
    PendingChats {
        users: Vec<PeerInfo>,
    },
    /// Peer became reachable. `status` carries the peer's current status so
    /// the receiver can render `busy` / `away` correctly right away instead
    /// of assuming `online`.
    PeerOnline {
        user_id: i64,
        status: UserStatus,
    },
    PeerOffline {
        user_id: i64,
    },
    StatusUpdate {
        user_id: i64,
        status: UserStatus,
    },
    Message {
        id: String,
        from: i64,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    NoteMessage {
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    PullHistoryRequest {
        from: i64,
        since: i64,
        #[serde(default)]
        limit: Option<u32>,
        #[serde(default)]
        before: Option<i64>,
    },
    HistoryResponse {
        from: i64,
        messages: Vec<StoredMsg>,
    },
    ReadReceipt {
        from: i64,
        up_to_ts: i64,
    },
    Error {
        msg: String,
    },
    Close {
        reason: String,
    },
    ChatLeft {
        peer: i64,
    },
    Blocked {
        users: Vec<PeerInfo>,
    },
    Profile {
        user_id: i64,
        username: String,
        #[serde(default)]
        display_name: Option<String>,
        #[serde(default)]
        avatar: Option<String>,
    },
}