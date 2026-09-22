use tokio::sync::{mpsc, oneshot, Mutex};

use crate::db::Database;
use crate::protocol::ClientMsg;

pub struct AppState {
    pub inner: Mutex<Inner>,
}

pub struct Inner {
    pub db: Option<Database>,
    pub username: Option<String>,
    pub ws_tx: Option<mpsc::UnboundedSender<ClientMsg>>,
    pub shutdown_tx: Option<oneshot::Sender<()>>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Inner {
                db: None,
                username: None,
                ws_tx: None,
                shutdown_tx: None,
            }),
        }
    }
}
