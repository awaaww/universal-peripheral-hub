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
            fit_window(app.handle());
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

/// Keep the configured 1024x760 panel inside the monitor that will show it.
///
/// Tauri sizes windows in logical pixels, so on a 150–175 % display the
/// configured window is wider than the physical screen and the right-hand
/// controls (the ones that write to devices) end up off-screen. Clamp to the
/// monitor and centre, once, at startup.
fn fit_window(app: &tauri::AppHandle) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    let monitor = window.current_monitor().ok().flatten();
    let scale = monitor.as_ref().map(|m| m.scale_factor()).unwrap_or(1.0);
    let size = monitor.as_ref().map(|m| m.size().clone());
    let (physical_w, physical_h) = size
        .map(|s| (s.width as f64, s.height as f64))
        .unwrap_or((1280.0, 720.0));
    let logical_w = physical_w / scale;
    let logical_h = physical_h / scale;
    eprintln!(
        "panel: monitor {physical_w}x{physical_h} @ {scale}x (logical {logical_w}x{logical_h})"
    );
    // Room for the taskbar and the window frame.
    let max_w = (logical_w - 96.0).max(480.0);
    let max_h = (logical_h - 140.0).max(360.0);
    if let Ok(current) = window.outer_size() {
        let mut w = current.width as f64 / scale;
        let mut h = current.height as f64 / scale;
        if w > max_w || h > max_h {
            w = w.min(max_w);
            h = h.min(max_h);
            let _ = window.set_size(tauri::LogicalSize::new(w, h));
            eprintln!("panel: resized to {w}x{h} logical");
        }
    }
    let _ = window.center();
}
