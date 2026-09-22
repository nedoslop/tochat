mod auth;
mod db;
mod protocol;
mod state;
mod util;
mod ws;

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use dashmap::DashMap;

use db::Database;
use state::AppState;

#[tokio::main]
async fn main() {
    let db_path = std::env::var("CHAT_DB").unwrap_or_else(|_| "chat.db".into());
    let db = Database::open(&db_path).expect("failed to open database");

    let state = AppState {
        db,
        online: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        .route("/register", post(auth::register))
        .route("/login", post(auth::login))
        .route("/ws", get(ws::ws_handler))
        .with_state(state);

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".into());
    let addr = format!("0.0.0.0:{port}");
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    println!("server listening on {addr}");
    axum::serve(listener, app).await.unwrap();
}
