//! Single-instance guard.
//!
//! A named mutex rather than a lock file: Windows releases it when the process
//! dies, so a crash never leaves the hub permanently "already running".

use std::io;

use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS};
use windows_sys::Win32::System::Threading::CreateMutexW;

use crate::winutil::to_wide;

pub struct InstanceGuard {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

impl InstanceGuard {
    /// `Ok(Some(guard))` means this process owns the instance; `Ok(None)` means
    /// another `hubd` already runs in this session.
    pub fn acquire(name: &str) -> io::Result<Option<Self>> {
        let wide = to_wide(name);
        let handle = unsafe { CreateMutexW(std::ptr::null(), 0, wide.as_ptr()) };
        if handle == 0 {
            return Err(io::Error::last_os_error());
        }
        let last = unsafe { GetLastError() };
        if last == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle) };
            Ok(None)
        } else {
            Ok(Some(Self { handle }))
        }
    }
}

impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if self.handle != 0 {
            unsafe { CloseHandle(self.handle) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_acquire_reports_already_running() {
        let name = format!("Local\\uph-single-test-{}", std::process::id());
        let first = InstanceGuard::acquire(&name).unwrap();
        assert!(first.is_some());
        let second = InstanceGuard::acquire(&name).unwrap();
        assert!(second.is_none());
        drop(first);
        let third = InstanceGuard::acquire(&name).unwrap();
        assert!(third.is_some(), "mutex must be released with the guard");
    }
}
