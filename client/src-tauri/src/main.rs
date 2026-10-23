#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(windows)]
fn setup_instance_environment() {
    if std::env::var_os("WEBVIEW2_USER_DATA_FOLDER").is_some() {
        return;
    }

    let args: Vec<String> = std::env::args().collect();
    for i in 1..args.len() {
        if (args[i] == "--instance" || args[i] == "-i" || args[i] == "--profile") && i + 1 < args.len() {
            let id = &args[i + 1];
            if let Some(local) = std::env::var_os("LOCALAPPDATA") {
                let path = std::path::PathBuf::from(local)
                    .join("com.nedoslop.tochat")
                    .join(format!("EBWebView_{id}"));
                std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", path);
                return;
            }
        }
    }

    use std::ptr::null_mut;
    extern "system" {
        fn CreateMutexW(
            lpMutexAttributes: *mut std::ffi::c_void,
            bInitialOwner: i32,
            lpName: *const u16,
        ) -> *mut std::ffi::c_void;
        fn GetLastError() -> u32;
    }
    const ERROR_ALREADY_EXISTS: u32 = 183;

    let Some(local) = std::env::var_os("LOCALAPPDATA") else { return };

    for slot in 1..=16 {
        let name: Vec<u16> = format!("Local\\tochat_client_slot_{slot}\0").encode_utf16().collect();
        let handle = unsafe { CreateMutexW(null_mut(), 1, name.as_ptr()) };
        if !handle.is_null() && unsafe { GetLastError() } != ERROR_ALREADY_EXISTS {
            let _ = handle;
            let path = std::path::PathBuf::from(&local)
                .join("com.nedoslop.tochat")
                .join(format!("EBWebView_instance_{slot}"));
            std::env::set_var("WEBVIEW2_USER_DATA_FOLDER", path);
            return;
        }
    }
}

#[cfg(not(windows))]
fn setup_instance_environment() {}

fn main() {
    setup_instance_environment();
    chat_client::run();
}
