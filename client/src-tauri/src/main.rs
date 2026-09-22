#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod crypto;
mod db;
mod http_auth;
mod protocol;
mod state;
mod util;
mod ws;

use tauri::Manager;

use crate::db::Database;
use crate::state::AppState;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = Database::open(&dir.join("local.db"))?;
            app.manage(AppState::new(db));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::connect,
            commands::disconnect,
            commands::send_message,
            commands::pull_history,
            commands::list_pending,
            commands::delete_account,
            commands::set_peer_password,
            commands::get_messages,
            commands::wipe_local_data,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
