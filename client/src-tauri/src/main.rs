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

use crate::state::{read_theme, AppState};

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .setup(|app| {
            let dir = app.path().app_local_data_dir()?;
            std::fs::create_dir_all(&dir)?;

            let theme = read_theme(&dir);
            commands::apply_window_theme(app.handle(), &theme);

            let state = AppState::new(app.handle().clone(), dir);
            app.manage(Arc::new(state));
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::register,
            commands::connect,
            commands::disconnect,
            commands::send_message,
            commands::send_media,
            commands::edit_message,
            commands::delete_message,
            commands::pull_history,
            commands::mark_read,
            commands::list_pending,
            commands::delete_account,
            commands::get_messages,
            commands::wipe_local_data,
            commands::set_encryption,
            commands::get_encryption,
            commands::get_theme,
            commands::set_theme,
            commands::is_release,
            commands::set_status,
            commands::clear_chat,
            commands::leave_chat,
            commands::block_user,
            commands::unblock_user,
            commands::list_blocked,
            commands::generate_psk,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}