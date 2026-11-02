//! Per-model configuration and the user's block list.
//!
//! Two sources, both optional:
//!
//! * bundled `data/monitors/<KEY>.json` shipped with the repository,
//! * `%APPDATA%\UniversalPeripheralHub\monitors\<KEY>.json` written by the user.
//!
//! The user copy wins field by field. Unknown fields are ignored so a newer
//! data file does not break an older build.
//!
//! Blocked monitors are skipped during enumeration. Blocking exists because a
//! monitor behind a one-way HDMI splitter or a KVM can make every DDC request
//! wait for seconds and stall the shared I2C bus for the other monitors.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
pub struct MonitorConfig {
    #[serde(default)]
    pub settle_ms: Option<u64>,
    #[serde(default)]
    pub brightness_vcp: Option<u8>,
    #[serde(default)]
    pub quirks: Quirks,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Quirks {
    #[serde(default)]
    pub unsupported_vcp: Vec<u8>,
}

impl MonitorConfig {
    /// User configuration overrides the bundled file field by field.
    pub fn load(key: &str, config_dir: &Path, repo_data_dir: Option<&Path>) -> Self {
        let mut config = repo_data_dir
            .and_then(|dir| read_file(&dir.join(format!("{key}.json"))))
            .unwrap_or_default();
        if let Some(user) = read_file(&config_dir.join("monitors").join(format!("{key}.json"))) {
            if user.settle_ms.is_some() {
                config.settle_ms = user.settle_ms;
            }
            if user.brightness_vcp.is_some() {
                config.brightness_vcp = user.brightness_vcp;
            }
            if !user.quirks.unsupported_vcp.is_empty() {
                config.quirks.unsupported_vcp = user.quirks.unsupported_vcp;
            }
        }
        config
    }
}

fn read_file(path: &Path) -> Option<MonitorConfig> {
    let text = std::fs::read_to_string(path).ok()?;
    match serde_json::from_str(&text) {
        Ok(config) => Some(config),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "ignoring unreadable monitor config");
            None
        }
    }
}

/// Monitor keys the user asked us to leave alone (`ddc_blocked` in settings).
pub fn blocked_keys(settings_path: &Path) -> Vec<String> {
    #[derive(Deserialize)]
    struct Partial {
        #[serde(default)]
        ddc_blocked: Vec<String>,
    }

    let Ok(text) = std::fs::read_to_string(settings_path) else {
        return Vec::new();
    };
    serde_json::from_str::<Partial>(&text)
        .map(|partial| {
            partial
                .ddc_blocked
                .into_iter()
                .map(|key| key.to_ascii_uppercase())
                .collect()
        })
        .unwrap_or_default()
}

/// `%APPDATA%\UniversalPeripheralHub` — the same directory `hubd` uses.
pub fn default_config_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("UniversalPeripheralHub")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_monitor_config() {
        let config: MonitorConfig = serde_json::from_str(
            r#"{
                "schema": 1,
                "name": "KTC H27T22S",
                "settle_ms": 200,
                "brightness_vcp": 16,
                "quirks": { "unsupported_vcp": [233] }
            }"#,
        )
        .expect("valid config");
        assert_eq!(config.settle_ms, Some(200));
        assert_eq!(config.brightness_vcp, Some(16));
        assert_eq!(config.quirks.unsupported_vcp, vec![233]);
    }

    #[test]
    fn blocked_keys_are_uppercased() {
        let dir = std::env::temp_dir().join(format!("uph-blocked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("settings.json");
        std::fs::write(&path, r#"{"ddc_blocked":["skg2725"]}"#).expect("write");
        assert_eq!(blocked_keys(&path), vec!["SKG2725".to_string()]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_settings_means_nothing_blocked() {
        assert!(blocked_keys(Path::new("no-such-file.json")).is_empty());
    }
}
