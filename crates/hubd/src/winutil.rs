//! Small Win32 helpers shared by the binary-only modules.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

pub fn to_wide(value: impl AsRef<OsStr>) -> Vec<u16> {
    value
        .as_ref()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Copy a Rust string into a fixed-size UTF-16 field, truncating if needed and
/// always NUL-terminating.
pub fn copy_wide<const N: usize>(target: &mut [u16; N], value: &str) {
    let wide: Vec<u16> = value.encode_utf16().take(N - 1).collect();
    target.fill(0);
    target[..wide.len()].copy_from_slice(&wide);
}

/// Hide the console window of this process.
///
/// `hubd` is a console subsystem binary so that `--selftest` and friends can
/// print to a terminal. When the installer or the Run key starts it with no
/// arguments there is no terminal to print to, and Windows would otherwise leave
/// a black console window on screen for the lifetime of the process.
pub fn hide_console_window() {
    use windows_sys::Win32::System::Console::GetConsoleWindow;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ShowWindow, SW_HIDE};

    let console = unsafe { GetConsoleWindow() };
    if console != 0 {
        unsafe {
            ShowWindow(console, SW_HIDE);
        }
    }
}
