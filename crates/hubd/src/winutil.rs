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
