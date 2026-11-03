//! Win32 notification-area icon and its menu.
//!
//! Deliberately dependency-free (no tray crate) and owned by `hubd` rather than
//! the Tauri shell: the icon must exist while the panel is closed, and the
//! panel is allowed to be closed at any time.

use std::io;
use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::{mpsc, Arc, OnceLock};
use std::time::Duration;

use tokio::sync::watch;
use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::UI::Shell::{
    Shell_NotifyIconW, NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_TIP, NIIF_INFO, NIM_ADD, NIM_DELETE,
    NIM_MODIFY, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreateIconFromResourceEx, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
    DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW,
    LoadIconW, PostMessageW, PostQuitMessage, RegisterClassExW, SetForegroundWindow,
    TrackPopupMenu, TranslateMessage, MF_CHECKED, MF_SEPARATOR, MF_STRING, MSG, TPM_NONOTIFY,
    TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND, WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP,
    WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
};

use crate::service::Service;
use crate::winutil::{copy_wide, to_wide};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const ID_OPEN: u32 = 1001;
const ID_TOGGLE: u32 = 1002;
const ID_AUTOSTART: u32 = 1003;
const ID_QUIT: u32 = 1004;

struct TrayContext {
    service: Arc<Service>,
    shutdown: watch::Sender<bool>,
}

static CONTEXT: OnceLock<Arc<TrayContext>> = OnceLock::new();
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);

/// Create the icon on a dedicated thread and return once it is visible, so the
/// caller can report a tray failure instead of pretending it worked.
pub fn spawn(service: Arc<Service>, shutdown: watch::Sender<bool>) -> io::Result<()> {
    let _ = CONTEXT.set(Arc::new(TrayContext { service, shutdown }));
    let (ready_tx, ready_rx) = mpsc::channel::<io::Result<()>>();

    std::thread::Builder::new()
        .name("tray".into())
        .spawn(move || thread_main(ready_tx))?;

    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "the tray thread did not report readiness",
        )),
    }
}

fn thread_main(ready: mpsc::Sender<io::Result<()>>) {
    match unsafe { create_window() } {
        Ok(hwnd) => {
            TRAY_HWND.store(hwnd, Ordering::Relaxed);
            let _ = ready.send(Ok(()));
            unsafe { message_loop() };
            remove_icon(hwnd);
            unsafe {
                DestroyWindow(hwnd);
            }
        }
        Err(err) => {
            let _ = ready.send(Err(err));
        }
    }
}

unsafe fn create_window() -> io::Result<HWND> {
    let instance = GetModuleHandleW(std::ptr::null());
    let class_name = to_wide("UniversalPeripheralHubTray");

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: 0,
        lpfnWndProc: Some(wndproc),
        cbClsExtra: 0,
        cbWndExtra: 0,
        hInstance: instance,
        hIcon: 0,
        hCursor: LoadCursorW(0, 32512 as *const u16),
        hbrBackground: 0,
        lpszMenuName: std::ptr::null(),
        lpszClassName: class_name.as_ptr(),
        hIconSm: 0,
    };
    if RegisterClassExW(&wc) == 0 {
        // The class may already exist if the tray thread was restarted; only a
        // failed window creation is fatal.
        tracing::debug!("tray window class already registered");
    }

    let title = to_wide("Universal Peripheral Hub");
    let hwnd = CreateWindowExW(
        0,
        class_name.as_ptr(),
        title.as_ptr(),
        0,
        0,
        0,
        0,
        0,
        0,
        0,
        instance,
        std::ptr::null(),
    );
    if hwnd == 0 {
        return Err(io::Error::last_os_error());
    }

    let icon = app_icon();
    if !add_icon(hwnd, icon) {
        return Err(io::Error::other("Shell_NotifyIconW(NIM_ADD) failed"));
    }
    Ok(hwnd)
}

unsafe fn message_loop() {
    let mut message: MSG = std::mem::zeroed();
    loop {
        let result = GetMessageW(&mut message, 0, 0, 0);
        if result == -1 || result == 0 {
            break;
        }
        TranslateMessage(&message);
        DispatchMessageW(&message);
    }
}

unsafe extern "system" fn wndproc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        TRAY_MESSAGE => {
            match (lparam & 0xFFFF) as u32 {
                WM_LBUTTONUP => activate(),
                WM_RBUTTONUP | WM_CONTEXTMENU => show_menu(hwnd),
                _ => {}
            }
            0
        }
        WM_COMMAND => {
            match (wparam & 0xFFFF) as u32 {
                ID_OPEN => activate(),
                ID_TOGGLE => toggle_mock(),
                ID_AUTOSTART => toggle_autostart(),
                ID_QUIT => quit(),
                _ => {}
            }
            0
        }
        WM_DESTROY => {
            PostQuitMessage(0);
            0
        }
        _ => DefWindowProcW(hwnd, message, wparam, lparam),
    }
}

fn activate() {
    if let Some(context) = CONTEXT.get() {
        context.service.activate();
    }
}

fn toggle_mock() {
    let Some(context) = CONTEXT.get() else { return };
    let enabled = context
        .service
        .snapshot()
        .adapters
        .iter()
        .find(|adapter| adapter.id == "mock")
        .map(|adapter| adapter.enabled)
        .unwrap_or(true);
    if let Err(err) = context.service.set_enabled("mock", !enabled) {
        tracing::warn!(error = %err, "cannot toggle the mock adapter");
    }
}

/// Flips the per-user Run value. The tray is the only UI for this in M6, so a
/// failure has to be visible in the log (and the menu re-reads the registry on
/// the next open, which shows the true state).
fn toggle_autostart() {
    let enabled = crate::autostart::is_enabled();
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(err) => {
            tracing::warn!(error = %err, "cannot resolve the hubd path for autostart");
            return;
        }
    };
    match crate::autostart::set(!enabled, &exe) {
        Ok(now) => tracing::info!(enabled = now, "autostart updated"),
        Err(err) => tracing::warn!(error = %err, "cannot update the autostart value"),
    }
}

fn quit() {
    if let Some(hwnd) = hwnd() {
        remove_icon(hwnd);
        unsafe {
            PostMessageW(hwnd, WM_NULL, 0, 0);
            DestroyWindow(hwnd);
        }
    }
    if let Some(context) = CONTEXT.get() {
        let _ = context.shutdown.send(true);
    }
}

fn hwnd() -> Option<HWND> {
    let raw = TRAY_HWND.load(Ordering::Relaxed);
    (raw != 0).then_some(raw)
}

unsafe fn show_menu(hwnd: HWND) {
    let menu = CreatePopupMenu();
    if menu == 0 {
        return;
    }

    let language = current_language();
    let labels = labels(&language);
    let open = to_wide(labels.open);
    AppendMenuW(menu, MF_STRING, ID_OPEN as usize, open.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());

    let enabled = CONTEXT
        .get()
        .map(|context| {
            context
                .service
                .snapshot()
                .adapters
                .iter()
                .find(|adapter| adapter.id == "mock")
                .map(|adapter| adapter.enabled)
                .unwrap_or(true)
        })
        .unwrap_or(true);
    let toggle = to_wide(if enabled {
        labels.disable_mock
    } else {
        labels.enable_mock
    });
    AppendMenuW(menu, MF_STRING, ID_TOGGLE as usize, toggle.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    let autostart = to_wide(labels.autostart);
    let autostart_flags = if crate::autostart::is_enabled() {
        MF_STRING | MF_CHECKED
    } else {
        MF_STRING
    };
    AppendMenuW(
        menu,
        autostart_flags,
        ID_AUTOSTART as usize,
        autostart.as_ptr(),
    );
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    let quit_label = to_wide(labels.quit);
    AppendMenuW(menu, MF_STRING, ID_QUIT as usize, quit_label.as_ptr());

    let mut point: POINT = std::mem::zeroed();
    if GetCursorPos(&mut point) == 0 {
        DestroyMenu(menu);
        return;
    }
    // Required so the menu closes when the user clicks elsewhere.
    SetForegroundWindow(hwnd);
    let command = TrackPopupMenu(
        menu,
        TPM_RIGHTBUTTON | TPM_RETURNCMD | TPM_NONOTIFY,
        point.x,
        point.y,
        0,
        hwnd,
        std::ptr::null(),
    );
    PostMessageW(hwnd, WM_NULL, 0, 0);
    DestroyMenu(menu);

    match command as u32 {
        ID_OPEN => activate(),
        ID_TOGGLE => toggle_mock(),
        ID_QUIT => quit(),
        _ => {}
    }
}

/// Our own icon, embedded from the repository so the installed binary has no
/// runtime dependency on a file next to it. Falls back to the stock application
/// icon when the ICO cannot be parsed, because a tray without an icon is worse
/// than a generic one.
const APP_ICON: &[u8] = include_bytes!("../../../ui/src-tauri/icons/icon.ico");

fn app_icon() -> isize {
    match unsafe { icon_from_ico(APP_ICON) } {
        Some(icon) => {
            tracing::debug!(bytes = APP_ICON.len(), "using the embedded tray icon");
            icon
        }
        None => {
            tracing::warn!(
                bytes = APP_ICON.len(),
                "embedded icon could not be parsed; falling back to the stock icon"
            );
            unsafe { LoadIconW(0, 32512 as *const u16) }
        }
    }
}

unsafe fn icon_from_ico(bytes: &[u8]) -> Option<isize> {
    if bytes.len() < 6 {
        return None;
    }
    let kind = u16::from_le_bytes([bytes[2], bytes[3]]);
    let count = u16::from_le_bytes([bytes[4], bytes[5]]) as usize;
    if kind != 1 || count == 0 {
        return None;
    }
    // Pick the largest image; the tray scales it down, and a 16x16 entry would
    // look blurred on high-DPI displays.
    let mut best: Option<(u32, usize, usize, u32, u32)> = None;
    for index in 0..count {
        let at = 6 + index * 16;
        if at + 16 > bytes.len() {
            break;
        }
        let width = if bytes[at] == 0 {
            256
        } else {
            u32::from(bytes[at])
        };
        let height = if bytes[at + 1] == 0 {
            256
        } else {
            u32::from(bytes[at + 1])
        };
        let length =
            u32::from_le_bytes([bytes[at + 8], bytes[at + 9], bytes[at + 10], bytes[at + 11]]);
        let offset = u32::from_le_bytes([
            bytes[at + 12],
            bytes[at + 13],
            bytes[at + 14],
            bytes[at + 15],
        ]);
        let end = offset as usize + length as usize;
        if end > bytes.len() {
            continue;
        }
        let area = width * height;
        if best.map(|(best_area, ..)| area > best_area).unwrap_or(true) {
            best = Some((area, offset as usize, length as usize, width, height));
        }
    }
    let (_, offset, length, width, height) = best?;
    let icon = CreateIconFromResourceEx(
        bytes[offset..offset + length].as_ptr(),
        length as u32,
        1,
        0x0003_0000,
        width as i32,
        height as i32,
        0,
    );
    (icon != 0).then_some(icon)
}

struct Labels {
    open: &'static str,
    disable_mock: &'static str,
    enable_mock: &'static str,
    autostart: &'static str,
    quit: &'static str,
}

fn labels(language: &str) -> Labels {
    match language {
        "zh-CN" => Labels {
            open: "打开控制面板",
            disable_mock: "停用模拟适配器",
            enable_mock: "启用模拟适配器",
            autostart: "开机时启动",
            quit: "退出通用外设中心",
        },
        "zh-TW" => Labels {
            open: "開啟控制面板",
            disable_mock: "停用模擬適配器",
            enable_mock: "啟用模擬適配器",
            autostart: "開機時啟動",
            quit: "結束通用外設中心",
        },
        _ => Labels {
            open: "Open panel",
            disable_mock: "Disable Mock adapter",
            enable_mock: "Enable Mock adapter",
            autostart: "Start with Windows",
            quit: "Quit Universal Peripheral Hub",
        },
    }
}

fn current_language() -> String {
    CONTEXT
        .get()
        .map(|context| context.service.prefs().language)
        .unwrap_or_else(|| "en".to_string())
}

/// Show a notification-area balloon. Called from any thread: the icon handle is
/// global and `Shell_NotifyIconW` is thread-agnostic for an existing icon.
pub fn balloon(title: &str, body: &str) {
    let Some(hwnd) = hwnd() else { return };
    let mut data: NOTIFYICONDATAW = unsafe { std::mem::zeroed() };
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_INFO;
    copy_wide(&mut data.szInfoTitle, title);
    copy_wide(&mut data.szInfo, body);
    data.dwInfoFlags = NIIF_INFO;
    data.Anonymous.uTimeout = 10_000;
    let ok = unsafe { Shell_NotifyIconW(NIM_MODIFY, &data) } != 0;
    if ok {
        tracing::info!(title, "balloon notification accepted by the shell");
    } else {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        tracing::warn!(
            error = code,
            "Shell_NotifyIconW(NIM_MODIFY/NIF_INFO) failed"
        );
    }
}

/// Localized low-battery notification, so the tray and the panel speak the same
/// language as the preference.
pub fn battery_alert(language: &str, device: &str, percent: u8) {
    let (title, body) = match language {
        "zh-CN" => ("电量偏低", format!("{device}：剩余 {percent}%")),
        "zh-TW" => ("電量偏低", format!("{device}：剩餘 {percent}%")),
        _ => ("Battery low", format!("{device}: {percent}% remaining")),
    };
    tracing::info!(device, percent, "battery alert");
    balloon(title, &body);
}

unsafe fn add_icon(hwnd: HWND, icon: isize) -> bool {
    let mut data: NOTIFYICONDATAW = std::mem::zeroed();
    data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    data.hWnd = hwnd;
    data.uID = 1;
    data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    data.uCallbackMessage = TRAY_MESSAGE;
    data.hIcon = icon;
    copy_wide(&mut data.szTip, "Universal Peripheral Hub");
    let ok = Shell_NotifyIconW(NIM_ADD, &data) != 0;
    if !ok {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
        tracing::warn!(
            error = code,
            cb_size = data.cbSize,
            "Shell_NotifyIconW(NIM_ADD) failed"
        );
        if code == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32 {
            // UIPI blocks WM_COPYDATA when the caller runs at a lower integrity
            // level than the shell, which happens when the executable lives in a
            // folder that carries a Low mandatory label (sandboxed build trees do
            // this): the image lowers the process token. The hub still works, it
            // just cannot draw a tray icon from there.
            tracing::warn!(
                "the taskbar rejected the icon because this process runs at a lower integrity \
                 level than the shell; move the build outside a Low-integrity folder so the \
                 tray icon can be added"
            );
        }
    }
    ok
}

fn remove_icon(hwnd: HWND) {
    unsafe {
        let mut data: NOTIFYICONDATAW = std::mem::zeroed();
        data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
        data.hWnd = hwnd;
        data.uID = 1;
        Shell_NotifyIconW(NIM_DELETE, &data);
    }
}
