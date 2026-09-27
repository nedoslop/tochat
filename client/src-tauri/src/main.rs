#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod commands;
mod crypto;
mod db;
mod protocol;
mod state;
mod util;
mod ws;

use std::sync::Arc;
use tauri::Manager;

use crate::state::AppState;

/// Application entry point: sets up state and registers commands.
fn main() {
    tauri::Builder::default()
        .setup(|app| {
            let dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let state = AppState::new(app.handle().clone(), dir);
            app.manage(Arc::new(state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::register,
            commands::connect,
            commands::disconnect,
            commands::send_message,
            commands::edit_message,
            commands::delete_message,
            commands::pull_history,
            commands::list_pending,
            commands::delete_account,
            commands::get_messages,
            commands::wipe_local_data,
            commands::set_encryption,
            commands::get_encryption,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
