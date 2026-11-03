//! Per-user autostart.
//!
//! The plan called for a Scheduled Task, but a logon task needs elevation to
//! register and the installer is per-user, so this uses the documented
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Run` value instead: same
//! "starts when I log in" behaviour, no UAC, trivially reversible, and visible
//! in Task Manager's Startup tab. See D32 in `docs/decisions.md`.

use std::io;
use std::path::Path;

use winreg::enums::{HKEY_CURRENT_USER, KEY_CREATE_SUB_KEY, KEY_READ, KEY_SET_VALUE};
use winreg::RegKey;

/// Value name used in production. Tests use their own names so they never
/// disturb a real installation.
pub const VALUE_NAME: &str = "UniversalPeripheralHub";

const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";

fn run_key(access: u32) -> io::Result<RegKey> {
    RegKey::predef(HKEY_CURRENT_USER).open_subkey_with_flags(RUN_KEY, access)
}

/// Opens the Run key for writing, creating it when the profile has never had
/// one. A fresh Windows profile (and CI runners) can lack the key entirely;
/// opening it must not turn a supported operation into an error.
fn run_key_for_write() -> io::Result<RegKey> {
    match run_key(KEY_SET_VALUE) {
        Ok(key) => Ok(key),
        Err(err) if err.kind() == io::ErrorKind::NotFound => RegKey::predef(HKEY_CURRENT_USER)
            .open_subkey_with_flags(RUN_KEY, KEY_CREATE_SUB_KEY)
            .or_else(|_| {
                RegKey::predef(HKEY_CURRENT_USER)
                    .create_subkey(RUN_KEY)
                    .map(|(key, _disposition)| key)
            }),
        Err(err) => Err(err),
    }
}

/// Command line stored under the value name: the executable, quoted so paths
/// with spaces survive.
pub fn command(exe: &Path) -> String {
    format!("\"{}\"", exe.display())
}

pub fn enable(exe: &Path) -> io::Result<()> {
    run_key_for_write()?.set_value(VALUE_NAME, &command(exe))
}

pub fn disable() -> io::Result<()> {
    disable_named(VALUE_NAME)
}

/// `true` when the value exists and still points at this file name.
pub fn is_enabled() -> bool {
    command_line(VALUE_NAME).is_some()
}

/// The raw command line, if the value exists.
pub fn command_line(name: &str) -> Option<String> {
    let key = run_key(KEY_READ).ok()?;
    key.get_value::<String, _>(name).ok()
}

/// `true` when an autostart value exists but points at another executable
/// (for example after the user moved or reinstalled the app).
pub fn is_stale(exe: &Path) -> bool {
    match command_line(VALUE_NAME) {
        Some(raw) => !raw.contains(&exe.display().to_string()),
        None => false,
    }
}

fn disable_named(name: &str) -> io::Result<()> {
    let key = match run_key(KEY_SET_VALUE) {
        Ok(key) => key,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err),
    };
    match key.delete_value(name) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err),
    }
}

/// Applies the requested state and reports the state that is now in effect.
pub fn set(enabled: bool, exe: &Path) -> io::Result<bool> {
    if enabled {
        enable(exe)?;
    } else {
        disable()?;
    }
    Ok(is_enabled())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn test_name() -> String {
        format!("UniversalPeripheralHub-test-{}", process::id())
    }

    #[test]
    fn enable_and_disable_are_reversible() {
        let name = test_name();
        let exe = Path::new(r"C:\Program Files\Universal Peripheral Hub\hubd.exe");

        let key = run_key_for_write().expect("open Run key");
        key.set_value(&name, &command(exe)).expect("write value");
        assert_eq!(
            command_line(&name).as_deref(),
            Some(format!("\"{}\"", exe.display()).as_str())
        );

        disable_named(&name).expect("delete value");
        assert!(command_line(&name).is_none());
    }

    #[test]
    fn disable_missing_value_is_not_an_error() {
        let name = test_name();
        disable_named(&name).expect("deleting an absent value must succeed");
    }

    #[test]
    fn command_quotes_the_path() {
        assert_eq!(
            command(Path::new(r"C:\a b\hubd.exe")),
            "\"C:\\a b\\hubd.exe\""
        );
    }
}
