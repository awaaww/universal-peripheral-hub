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
    Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
    DispatchMessageW, GetCursorPos, GetMessageW, LoadCursorW, LoadIconW, PostMessageW,
    PostQuitMessage, RegisterClassExW, SetForegroundWindow, TrackPopupMenu, TranslateMessage,
    MF_SEPARATOR, MF_STRING, MSG, TPM_NONOTIFY, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_COMMAND,
    WM_CONTEXTMENU, WM_DESTROY, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSEXW,
};

use crate::service::Service;
use crate::winutil::{copy_wide, to_wide};

const TRAY_MESSAGE: u32 = WM_APP + 1;
const ID_OPEN: u32 = 1001;
const ID_TOGGLE: u32 = 1002;
const ID_QUIT: u32 = 1003;

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

    let icon = LoadIconW(0, 32512 as *const u16);
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

    let open = to_wide("Open panel");
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
        "Disable Mock adapter"
    } else {
        "Enable Mock adapter"
    });
    AppendMenuW(menu, MF_STRING, ID_TOGGLE as usize, toggle.as_ptr());
    AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
    let quit_label = to_wide("Quit Universal Peripheral Hub");
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
