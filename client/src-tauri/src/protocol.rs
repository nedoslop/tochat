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

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::File => "file",
        }
    }
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

impl UserStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Away => "away",
            Self::Busy => "busy",
            Self::Invisible => "invisible",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "away" => Self::Away,
            "busy" => Self::Busy,
            "invisible" => Self::Invisible,
            _ => Self::Online,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    pub id: i64,
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: i64,
        id: String,
        ts: i64,
        kind: MessageKind,
        payload: String,
    },
    Edit {
        to: i64,
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    SendToSelf {
        id: String,
        ts: i64,
        edit_ts: i64,
        kind: MessageKind,
        payload: String,
    },
    PullHistory {
        from: i64,
        since: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
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
    GetProfile {
        username: String,
    },
    SetProfile {
        display_name: Option<String>,
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

#[derive(Debug, Clone, Deserialize)]
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