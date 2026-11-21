use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Serde default for flags that must be `true` when absent (older peers).
fn default_true() -> bool {
    true
}

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
    // NOTE: `history_response` is deliberately NOT part of this enum.
    // It is parsed separately as `HistoryResponseMsg` so the message
    // bodies stay as raw JSON (see below). Internally-tagged enums
    // buffer content during deserialization, which breaks RawValue.
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

/// A history reply, parsed as a PLAIN struct (not via the tagged enum).
///
/// The server never inspects message bodies — it only routes them — so
/// they are kept as `RawValue`: no deserialization of up-to-8-MiB base64
/// strings per part, and no re-serialization on relay. The requester's
/// client validates `from`/`to` of every message anyway, which is why
/// the server needs no typed `StoredMsg` at all.
#[derive(Debug, Deserialize)]
pub struct HistoryResponseMsg {
    pub to: i64,
    pub messages: Vec<Box<RawValue>>,
    /// `false` for intermediate parts of a multi-part history reply,
    /// `true` for the final (or only) part.
    #[serde(default = "default_true")]
    pub done: bool,
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
        /// Raw JSON bodies, relayed verbatim (zero parse / zero re-encode).
        messages: Vec<Box<RawValue>>,
        done: bool,
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