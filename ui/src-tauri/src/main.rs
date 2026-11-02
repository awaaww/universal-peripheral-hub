//! Tauri shell: a window onto `hubd`. It owns no device state, and closing it
//! does not stop the hub — the tray icon keeps running.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod hub;

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        // Must be the first plugin: a second launch only focuses the first
        // window instead of creating another panel.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.unminimize();
                let _ = window.show();
                let _ = window.set_focus();
            }
        }))
        .invoke_handler(tauri::generate_handler![hub::hub_request, hub::hub_status])
        .setup(|app| {
            hub::set_app(app.handle().clone());
            // Start the resident core early; the panel shows an offline banner
            // if this fails, and every later request retries on its own.
            std::thread::spawn(|| {
                if let Err(err) = hub::ensure() {
                    eprintln!("hubd unavailable: {}", err.message);
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running the Universal Peripheral Hub panel");
}
