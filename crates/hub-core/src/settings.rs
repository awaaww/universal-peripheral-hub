use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::{HubError, Result};

pub const SCHEMA_VERSION: u32 = 1;

/// Persisted user settings. Kept deliberately small: device state is owned by
/// the devices, not by this file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub schema_version: u32,
    /// Adapters the user switched off. Disabled adapters are not loaded at
    /// startup and are not retried until re-enabled.
    pub disabled_adapters: Vec<String>,
    /// Monitors (by EDID key, e.g. `SKG2725`) that must not be touched over
    /// DDC. A monitor behind a one-way splitter or a KVM can make every request
    /// wait seconds and stall the shared I2C bus for the other outputs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ddc_blocked: Vec<String>,
    /// Panel/tray preferences (language, auto scan, battery alerts, close
    /// button). Defaults are applied by serde when the file predates them.
    #[serde(default)]
    pub prefs: crate::prefs::Prefs,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            disabled_adapters: Vec::new(),
            ddc_blocked: Vec::new(),
            prefs: crate::prefs::Prefs::default(),
        }
    }
}

/// `%APPDATA%\UniversalPeripheralHub`, the directory used by every component.
pub fn default_config_dir() -> PathBuf {
    match std::env::var_os("APPDATA") {
        Some(appdata) => PathBuf::from(appdata).join("UniversalPeripheralHub"),
        None => PathBuf::from(".").join("UniversalPeripheralHub"),
    }
}

/// Settings file with atomic writes and corrupt-file quarantine.
pub struct SettingsStore {
    path: PathBuf,
    settings: Settings,
    quarantined: Option<PathBuf>,
}

impl SettingsStore {
    /// Load settings, never failing: a missing file yields defaults, and a file
    /// that cannot be parsed is preserved next to the original before defaults
    /// take over. Losing a user's settings silently is worse than a stray file.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let mut quarantined = None;
        let settings = match std::fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Settings>(&bytes) {
                Ok(settings) => settings,
                Err(err) => {
                    tracing::warn!(
                        path = %path.display(),
                        error = %err,
                        "settings file is not valid JSON; quarantining it"
                    );
                    quarantined = quarantine(&path).ok();
                    Settings::default()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Settings::default(),
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot read settings");
                Settings::default()
            }
        };
        Self {
            path,
            settings,
            quarantined,
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn settings_mut(&mut self) -> &mut Settings {
        &mut self.settings
    }

    /// Path of the preserved unparsable file, if one was quarantined on load.
    pub fn quarantined(&self) -> Option<&Path> {
        self.quarantined.as_deref()
    }

    pub fn update(&mut self, mutate: impl FnOnce(&mut Settings)) -> Result<()> {
        mutate(&mut self.settings);
        self.settings.schema_version = SCHEMA_VERSION;
        self.save()
    }

    /// Write via a temporary file and rename, so a crash never leaves a
    /// half-written settings file behind.
    pub fn save(&self) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&self.settings)
            .map_err(|err| HubError::internal(format!("cannot serialise settings: {err}")))?;
        let tmp = self.path.with_extension("json.tmp");
        std::fs::write(&tmp, &json)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

fn quarantine(path: &Path) -> std::io::Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let mut target = path.with_file_name(format!(
        "{}.corrupt-{stamp}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("settings.json")
    ));
    let mut suffix = 1;
    while target.exists() {
        target = path.with_file_name(format!(
            "{}.corrupt-{stamp}-{suffix}",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("settings.json")
        ));
        suffix += 1;
    }
    std::fs::rename(path, &target)?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("uph-settings-test-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("settings.json")
    }

    #[test]
    fn missing_file_yields_defaults() {
        let store = SettingsStore::load(temp_path("missing"));
        assert_eq!(store.settings(), &Settings::default());
        assert!(store.quarantined().is_none());
    }

    #[test]
    fn save_round_trips() {
        let path = temp_path("roundtrip");
        let mut store = SettingsStore::load(&path);
        store
            .update(|settings| settings.disabled_adapters.push("mock".into()))
            .unwrap();
        let reloaded = SettingsStore::load(&path);
        assert_eq!(reloaded.settings().disabled_adapters, vec!["mock"]);
        assert!(path.exists());
        assert!(!path.with_extension("json.tmp").exists());
    }

    #[test]
    fn corrupt_file_is_preserved_and_defaults_are_used() {
        let path = temp_path("corrupt");
        std::fs::write(&path, b"{ this is not json").unwrap();
        let store = SettingsStore::load(&path);
        assert_eq!(store.settings(), &Settings::default());
        let quarantined = store.quarantined().expect("file is quarantined");
        assert!(quarantined.exists());
        assert_eq!(std::fs::read(quarantined).unwrap(), b"{ this is not json");
        assert!(!path.exists());
    }
}
