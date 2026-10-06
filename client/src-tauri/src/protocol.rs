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

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Send {
        to: String,
        id: String,
        ts: i64,
        kind: MessageKind,
        payload: String,
    },
    Edit {
        to: String,
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
        from: String,
        since: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        limit: Option<u32>,
        #[serde(skip_serializing_if = "Option::is_none")]
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
    ReadReceipt {
        to: String,
        up_to_ts: i64,
    },
}

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