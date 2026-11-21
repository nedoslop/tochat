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
use clap::Parser;
use dashmap::DashMap;
use mimalloc::MiMalloc;

use db::Database;
use state::AppState;

#[global_allocator]
static ALLOCATOR: MiMalloc = MiMalloc;

/// Command-line configuration.
///
/// Precedence: CLI flag > environment variable > default.
/// `CHAT_PORT` and `CHAT_DB` are the pre-existing env vars and still work;
/// `CHAT_HOST` is new (the bind host used to be hardcoded to 0.0.0.0).
#[derive(Debug, Parser)]
#[command(name = "chat-server", version, about = "WebSocket chat relay server")]
struct Args {
    /// Address to bind: an IP (e.g. 127.0.0.1, ::1) or hostname (e.g. localhost).
    #[arg(long, env = "CHAT_HOST", default_value = "0.0.0.0")]
    host: String,

    /// TCP port to listen on (0 = let the OS pick a free port).
    #[arg(short, long, env = "CHAT_PORT", default_value_t = 8080)]
    port: u16,

    /// Path to the SQLite database file.
    #[arg(long, env = "CHAT_DB", default_value = "chat.db")]
    db: String,
}

#[tokio::main]
async fn main() {
    let args = Args::parse();

    let db = Database::open(&args.db).expect("failed to open database");
    let state = AppState {
        db,
        online: Arc::new(DashMap::new()),
        user_status: Arc::new(DashMap::new()),
        pulls: Arc::new(DashMap::new()),
    };

    let app = Router::new()
        .route("/register", post(auth::register))
        .route("/login", get(ws::ws_handler))
        .with_state(state);

    // If the host is a literal IP, build a SocketAddr so IPv6 hosts get
    // bracketed correctly ("[::1]:8080"). Hostnames go through DNS as before.
    let addr = match args.host.parse::<std::net::IpAddr>() {
        Ok(ip) => std::net::SocketAddr::new(ip, args.port).to_string(),
        Err(_) => format!("{}:{}", args.host, args.port),
    };

    let mut listener = None;
    for _ in 0..50 {
        match tokio::net::TcpListener::bind(&addr).await {
            Ok(l) => {
                listener = Some(l);
                break;
            }
            Err(_) => tokio::time::sleep(std::time::Duration::from_millis(100)).await,
        }
    }
    let listener = listener.unwrap_or_else(|| panic!("failed to bind tcp listener on {addr}"));

    // Print the actually-bound address so `--port 0` shows the real port.
    let local = listener.local_addr().expect("failed to read local address");
    println!("server listening on {local}");

    axum::serve(listener, app).await.unwrap();
}
