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

use crate::state::AppState;

fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            // DB opens lazily inside `connect` — one file per username.
            app.manage(AppState::new());
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::register,
            commands::connect,
            commands::disconnect,
            commands::send_message,
            commands::pull_history,
            commands::list_pending,
            commands::delete_account,
            commands::set_peer_encryption,
            commands::get_peer_encryption,
            commands::get_messages,
            commands::wipe_local_data,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
