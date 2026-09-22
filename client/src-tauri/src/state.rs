use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::db::LocalDb;
use crate::protocol::ClientMsg;

pub struct Connection {
    pub username: String,
    pub base_url: String,
    pub tx: mpsc::UnboundedSender<ClientMsg>,
}

#[derive(Clone, Default)]
pub struct EncConfig {
    pub method: String,
    pub password: Option<String>,
}

#[derive(Clone, Default)]
pub struct AppState {
    pub conn: Arc<Mutex<Option<Connection>>>,
    pub db: Arc<Mutex<Option<LocalDb>>>,
    pub enc: Arc<Mutex<HashMap<String, EncConfig>>>,
}

impl AppState {
    pub fn new() -> Self {
        Self::default()
    }
}
