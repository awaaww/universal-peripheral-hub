//! Panel preferences that must survive a restart and are shared by the panel,
//! the tray and the auto-scan loop.
//!
//! They live in `settings.json` next to the adapter list so there is one file to
//! back up. Validation is deliberately forgiving: an unknown language falls back
//! to English and out-of-range numbers are clamped, because refusing to start
//! over a stray preference would be worse than fixing it silently.

use serde::{Deserialize, Serialize};

/// Languages the panel ships. Adding one means adding a dictionary in
/// `ui/src/i18n.ts`; the core only validates membership.
pub const LANGUAGES: [&str; 3] = ["en", "zh-CN", "zh-TW"];

/// Seconds between automatic adapter re-enumerations. `0` disables the timer.
pub const MIN_AUTO_SCAN_SECS: u32 = 15;
pub const MAX_AUTO_SCAN_SECS: u32 = 3600;
pub const DEFAULT_AUTO_SCAN_SECS: u32 = 60;
pub const DEFAULT_BATTERY_ALERT_PERCENT: u8 = 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Prefs {
    /// One of [`LANGUAGES`].
    pub language: String,
    /// What the window's close button does: hide to the tray, or quit both the
    /// panel and the core.
    pub close_to_tray: bool,
    /// `0` = auto scan off, otherwise 15..=3600 seconds.
    pub auto_scan_secs: u32,
    pub battery_alert: bool,
    /// Alert when a battery reports at or below this percentage.
    pub battery_alert_percent: u8,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            language: "en".to_string(),
            close_to_tray: true,
            auto_scan_secs: 0,
            battery_alert: false,
            battery_alert_percent: DEFAULT_BATTERY_ALERT_PERCENT,
        }
    }
}

impl Prefs {
    /// Clamp a value coming from disk or from the panel.
    pub fn normalized(mut self) -> Self {
        if !LANGUAGES.contains(&self.language.as_str()) {
            self.language = "en".to_string();
        }
        if self.auto_scan_secs != 0 {
            self.auto_scan_secs = self
                .auto_scan_secs
                .clamp(MIN_AUTO_SCAN_SECS, MAX_AUTO_SCAN_SECS);
        }
        self.battery_alert_percent = self.battery_alert_percent.clamp(1, 99);
        self
    }

    /// The interval the auto-scan loop should use, or `None` when disabled.
    pub fn scan_interval(&self) -> Option<std::time::Duration> {
        (self.auto_scan_secs >= MIN_AUTO_SCAN_SECS)
            .then(|| std::time::Duration::from_secs(u64::from(self.auto_scan_secs)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_conservative() {
        let prefs = Prefs::default();
        assert_eq!(prefs.language, "en");
        assert!(prefs.close_to_tray);
        assert_eq!(prefs.scan_interval(), None);
        assert!(!prefs.battery_alert);
        assert_eq!(prefs.battery_alert_percent, 20);
    }

    #[test]
    fn unknown_language_falls_back_to_english() {
        let prefs = Prefs {
            language: "klingon".into(),
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.language, "en");
    }

    #[test]
    fn numbers_are_clamped() {
        let prefs = Prefs {
            auto_scan_secs: 2,
            battery_alert_percent: 0,
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.auto_scan_secs, MIN_AUTO_SCAN_SECS);
        assert_eq!(prefs.battery_alert_percent, 1);

        let prefs = Prefs {
            auto_scan_secs: 99_999,
            battery_alert_percent: 200,
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.auto_scan_secs, MAX_AUTO_SCAN_SECS);
        assert_eq!(prefs.battery_alert_percent, 99);
    }

    #[test]
    fn zero_keeps_auto_scan_off() {
        let prefs = Prefs {
            auto_scan_secs: 0,
            ..Prefs::default()
        }
        .normalized();
        assert_eq!(prefs.scan_interval(), None);
    }
}
