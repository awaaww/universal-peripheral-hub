//! Foreground-application detection.
//!
//! Windows raises `EVENT_SYSTEM_FOREGROUND` as soon as the foreground window
//! changes. A `SetWinEventHook` hook is preferable to polling because it fires
//! exactly at the switch, costs nothing while the user works, and does not lag
//! behind the application that just came forward. Polling `GetForegroundWindow`
//! on a timer would wake the process forever, report the switch up to two
//! seconds late, and still miss an application that came and went between two
//! samples.
//!
//! `WINEVENT_OUTOFCONTEXT` delivers the callback on the thread that installed
//! the hook, but only while that thread pumps messages, so the watcher owns a
//! dedicated message-loop thread. When `SetWinEventHook` cannot be installed
//! (for example in a restricted desktop), the same thread falls back to a
//! `SetTimer` tick every two seconds and applies the identical de-duplication;
//! the fallback is logged at `warn!`. In both modes the callback receives the
//! lower-cased executable basename and only fires when the name changes.

use std::ffi::OsString;
use std::io;
use std::os::windows::ffi::OsStringExt;
use std::path::Path;
use std::sync::atomic::{AtomicIsize, AtomicU32, Ordering};
use std::sync::mpsc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INSUFFICIENT_BUFFER, HANDLE, HWND};
use windows_sys::Win32::System::Threading::{
    GetCurrentThreadId, OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32,
    PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows_sys::Win32::UI::Accessibility::{SetWinEventHook, UnhookWinEvent, HWINEVENTHOOK};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    DispatchMessageW, GetForegroundWindow, GetMessageW, GetWindowThreadProcessId, KillTimer,
    PeekMessageW, PostThreadMessageW, SetTimer, TranslateMessage, EVENT_SYSTEM_FOREGROUND, MSG,
    PM_NOREMOVE, WINEVENT_OUTOFCONTEXT, WM_QUIT, WM_TIMER,
};

/// Interval of the polling fallback used when the hook is unavailable.
const POLL_INTERVAL_MS: u32 = 2_000;
/// Timer id used with `SetTimer(0, ...)`; thread timers are per-thread, so a
/// small fixed value cannot collide with another part of the process.
const POLL_TIMER_ID: usize = 1;
/// `QueryFullProcessImageNameW` accepts at most a 32767-character Win32 path.
const MAX_IMAGE_PATH: usize = 32_768;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);

struct CallbackState {
    on_change: Box<dyn Fn(&str) + Send>,
    /// Last reported executable, used to suppress consecutive duplicates.
    last: Option<String>,
}

/// The WinEvent callback has no user-data parameter, so the live callback is
/// kept in one process-wide slot. Only one watcher may exist at a time.
static CALLBACK: Mutex<Option<CallbackState>> = Mutex::new(None);
/// Hook handle published by the watcher thread so `Drop` can unhook it.
static HOOK: AtomicIsize = AtomicIsize::new(0);
/// Watcher thread id, published before the hook is installed so `Drop` can post
/// `WM_QUIT` even if start-up is still in flight.
static THREAD_ID: AtomicU32 = AtomicU32::new(0);

/// Watches the foreground application and reports executable-name changes.
///
/// The callback runs on the watcher's message-loop thread, so it must be quick:
/// a slow callback delays every later foreground event. The watcher must stay
/// alive as long as changes are wanted; dropping it unhooks, stops its timer,
/// posts `WM_QUIT` and joins the thread.
pub struct ForegroundWatcher {
    thread: Option<JoinHandle<()>>,
}

impl ForegroundWatcher {
    /// Installs the hook. `on_change` is called with the lower-cased executable
    /// basename on the thread that runs the Win32 message loop.
    ///
    /// Only one watcher may be installed per process; a second call fails with
    /// `io::ErrorKind::AlreadyExists` until the first one is dropped.
    pub fn spawn(on_change: impl Fn(&str) + Send + 'static) -> io::Result<Self> {
        {
            let mut slot = CALLBACK
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if slot.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "a foreground watcher is already installed",
                ));
            }
            *slot = Some(CallbackState {
                on_change: Box::new(on_change),
                last: None,
            });
        }

        let (ready_tx, ready_rx) = mpsc::channel();
        let thread = match std::thread::Builder::new()
            .name("foreground-watcher".into())
            .spawn(move || thread_main(ready_tx))
        {
            Ok(thread) => thread,
            Err(error) => {
                clear_callback();
                return Err(error);
            }
        };

        match ready_rx.recv_timeout(STARTUP_TIMEOUT) {
            Ok(Ok(())) => Ok(Self {
                thread: Some(thread),
            }),
            Ok(Err(error)) => {
                let _ = thread.join();
                clear_callback();
                Err(error)
            }
            Err(_) => {
                // Start-up blocked somewhere unexpected; make the thread exit
                // instead of detaching it and leaking the hook.
                request_shutdown();
                let _ = thread.join();
                clear_callback();
                Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the foreground watcher thread did not start",
                ))
            }
        }
    }
}

impl Drop for ForegroundWatcher {
    fn drop(&mut self) {
        // Unhook first so no new callbacks can be queued, then wake the message
        // loop. The thread kills its own timer as it exits (a thread timer must
        // be killed by the thread that created it) and re-checks the hook for
        // the start-up-timeout path.
        request_shutdown();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        clear_callback();
    }
}

fn clear_callback() {
    let mut slot = CALLBACK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *slot = None;
}

fn request_shutdown() {
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        // SAFETY: `hook` was returned by SetWinEventHook and this is its only
        // owner; UnhookWinEvent is safe to call from any thread.
        unsafe { UnhookWinEvent(hook) };
    }
    let thread_id = THREAD_ID.swap(0, Ordering::SeqCst);
    if thread_id != 0 {
        // SAFETY: posting WM_QUIT to our own watcher thread is permitted from
        // any thread; the message loop stops when GetMessageW returns 0.
        unsafe { PostThreadMessageW(thread_id, WM_QUIT, 0, 0) };
    }
}

fn thread_main(ready: mpsc::Sender<io::Result<()>>) {
    // SAFETY: GetCurrentThreadId has no preconditions.
    let thread_id = unsafe { GetCurrentThreadId() };
    THREAD_ID.store(thread_id, Ordering::SeqCst);

    // Create this thread's message queue up front (GetMessageW/PeekMessageW
    // create it lazily) so a start-up-timeout shutdown can always be posted.
    // SAFETY: `message` is writable storage; PM_NOREMOVE leaves the queue as is.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    unsafe { PeekMessageW(&mut message, 0, 0, 0, PM_NOREMOVE) };

    // SAFETY: the callback is a valid `unsafe extern "system"` function, the
    // module handle is null as required by WINEVENT_OUTOFCONTEXT, and both id
    // filters are 0 (all processes/threads).
    let hook = unsafe {
        SetWinEventHook(
            EVENT_SYSTEM_FOREGROUND,
            EVENT_SYSTEM_FOREGROUND,
            0,
            Some(win_event_proc),
            0,
            0,
            WINEVENT_OUTOFCONTEXT,
        )
    };

    let mut timer = 0usize;
    if hook == 0 {
        let error = io::Error::last_os_error();
        tracing::warn!(
            error = %error,
            "SetWinEventHook is unavailable; the foreground watcher falls back to two-second polling"
        );
        // SAFETY: hwnd 0 creates a timer owned by this thread; a null callback
        // means WM_TIMER is posted to the thread's message queue instead.
        timer = unsafe { SetTimer(0, POLL_TIMER_ID, POLL_INTERVAL_MS, None) };
        if timer == 0 {
            let error = io::Error::last_os_error();
            tracing::warn!(error = %error, "SetTimer failed; the foreground watcher cannot start");
            let _ = ready.send(Err(error));
            return;
        }
    } else {
        HOOK.store(hook, Ordering::SeqCst);
    }

    let _ = ready.send(Ok(()));
    message_loop(timer);

    if timer != 0 {
        // SAFETY: the timer was created on this thread, and this is the thread
        // that must stop it. Queued WM_TIMER messages die with the queue.
        unsafe { KillTimer(0, timer) };
    }
    let hook = HOOK.swap(0, Ordering::SeqCst);
    if hook != 0 {
        // SAFETY: still the sole owner when Drop has not already unhooked it.
        unsafe { UnhookWinEvent(hook) };
    }
}

fn message_loop(timer: usize) {
    let mut message: MSG = unsafe { std::mem::zeroed() };
    loop {
        // SAFETY: `message` is valid storage and hwnd 0 retrieves thread
        // messages, which is how an out-of-context WinEvent is delivered.
        let result = unsafe { GetMessageW(&mut message, 0, 0, 0) };
        if result == -1 || result == 0 {
            break;
        }
        if timer != 0 && message.message == WM_TIMER && message.hwnd == 0 {
            report_foreground();
        }
        // SAFETY: `message` was filled in by GetMessageW just above.
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

unsafe extern "system" fn win_event_proc(
    _hook: HWINEVENTHOOK,
    event: u32,
    _hwnd: HWND,
    _object_id: i32,
    _child_id: i32,
    _event_thread: u32,
    _event_time: u32,
) {
    if event == EVENT_SYSTEM_FOREGROUND {
        report_foreground();
    }
}

fn report_foreground() {
    let executable = current_executable();

    let mut slot = CALLBACK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let Some(state) = slot.as_mut() else {
        return;
    };
    let Some(executable) = executable else {
        // The switch was observable but the process was not (UWP host, elevated
        // process we may not open). Forget the last name so returning to the
        // previous application is reported again rather than suppressed.
        state.last = None;
        return;
    };

    if state.last.as_deref() == Some(executable.as_str()) {
        return;
    }
    state.last = Some(executable.clone());
    tracing::debug!(exe = %executable, "foreground application changed");

    // A panicking callback must never unwind across the Win32 callback boundary.
    let on_change = &state.on_change;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        on_change(&executable);
    }));
    if result.is_err() {
        tracing::warn!(exe = %executable, "foreground change callback panicked");
    }
}

/// The executable that currently has the foreground, lower-cased basename.
///
/// Returns `None` when there is no foreground window or its process cannot be
/// queried (UWP host, elevated process we may not open, ...). This is the same
/// lookup the watcher callback performs; callers use it to establish a
/// baseline before relying on change events.
pub fn current_executable() -> Option<String> {
    // SAFETY: GetForegroundWindow has no preconditions; 0 means no foreground.
    let window = unsafe { GetForegroundWindow() };
    if window == 0 {
        return None;
    }

    let mut process_id = 0u32;
    // SAFETY: `process_id` is valid writable storage; the call only writes it.
    unsafe { GetWindowThreadProcessId(window, &mut process_id) };
    if process_id == 0 {
        return None;
    }

    // SAFETY: PROCESS_QUERY_LIMITED_INFORMATION is enough for the image name.
    // Failure is expected for protected processes and is handled as "unknown".
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if process == 0 {
        return None;
    }
    let _process = OwnedProcess(process);

    let mut capacity = 512usize;
    loop {
        let mut buffer = vec![0u16; capacity];
        let mut size = capacity as u32;
        // SAFETY: the buffer holds `size` elements and `size` points to valid
        // storage; the API writes at most that many UTF-16 code units.
        let ok = unsafe {
            QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, buffer.as_mut_ptr(), &mut size)
        };
        if ok != 0 {
            let written = (size as usize).min(buffer.len());
            return basename_of(&buffer[..written]);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_INSUFFICIENT_BUFFER as i32)
            && capacity < MAX_IMAGE_PATH
        {
            capacity = MAX_IMAGE_PATH;
            continue;
        }
        return None;
    }
}

fn basename_of(wide: &[u16]) -> Option<String> {
    let path = OsString::from_wide(wide);
    let name = Path::new(&path)
        .file_name()?
        .to_string_lossy()
        .to_ascii_lowercase();
    (!name.is_empty()).then_some(name)
}

/// Closes the process handle on every path, including early returns.
struct OwnedProcess(HANDLE);

impl Drop for OwnedProcess {
    fn drop(&mut self) {
        // SAFETY: the handle came from OpenProcess and is closed exactly once.
        unsafe { CloseHandle(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::ForegroundWatcher;
    use std::time::Duration;

    /// Manual smoke test: switch between two applications while it runs.
    /// `cargo test -p hubd --lib foreground:: -- --ignored --nocapture`
    #[test]
    #[ignore = "needs an interactive desktop and a human (or script) switching windows"]
    fn manual_foreground_switch_detection() {
        let (tx, rx) = std::sync::mpsc::channel::<String>();
        let watcher = ForegroundWatcher::spawn(move |exe| {
            println!("foreground -> {exe}");
            let _ = tx.send(exe.to_owned());
        })
        .expect("install the foreground watcher");

        println!("observing for 8 s; switch between two applications now");
        std::thread::sleep(Duration::from_secs(8));
        drop(watcher);

        let seen: Vec<String> = rx.try_iter().collect();
        println!("observed {} change(s)", seen.len());
        for pair in seen.windows(2) {
            assert_ne!(
                pair[0], pair[1],
                "consecutive duplicates must be suppressed"
            );
        }
    }
}
