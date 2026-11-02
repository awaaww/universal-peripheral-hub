//! Locating the UI executable.
//!
//! In an installed layout the UI sits next to `hubd.exe`. In a development
//! checkout it lives under `ui/src-tauri/target/<profile>/`, so both layouts are
//! searched before giving up. `UPH_UI_EXE` overrides everything.

use std::path::{Path, PathBuf};

const UI_BINARY_NAMES: [&str; 2] = ["UniversalPeripheralHub.exe", "universal-peripheral-hub.exe"];

pub fn find_ui_executable() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("UPH_UI_EXE") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            "UPH_UI_EXE does not point at a file; falling back to autodetection"
        );
    }

    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();

    // Six levels covers `target/debug`, `target/release`, the repository root
    // and the `ui/src-tauri` variants of both.
    for _ in 0..6 {
        for candidate in candidates(&dir) {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

fn candidates(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for name in UI_BINARY_NAMES {
        found.push(dir.join(name));
        for profile in ["debug", "release"] {
            found.push(dir.join("ui/src-tauri/target").join(profile).join(name));
            found.push(dir.join("target").join(profile).join(name));
        }
    }
    found.push(dir.join("bin").join(UI_BINARY_NAMES[0]));
    found.push(dir.join("bin").join(UI_BINARY_NAMES[1]));
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_include_sibling_installed_layout() {
        let dir = Path::new("C:\\Program Files\\Universal Peripheral Hub");
        let list = candidates(dir);
        assert!(list.contains(&dir.join("UniversalPeripheralHub.exe")));
        assert!(list
            .iter()
            .any(|path| path.ends_with("ui/src-tauri/target/debug/UniversalPeripheralHub.exe")));
    }
}
