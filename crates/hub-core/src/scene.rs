//! Scene documents: what to change, when to change it, and how much the engine
//! is allowed to do on its own.
//!
//! A scene names capabilities by their stable `cap_type` (see D2 in
//! `docs/decisions.md`), not by an adapter-local id, so the same scene keeps
//! working when a device is replaced by another one of the same kind. An item
//! may pin a specific device when two monitors must not move together.
//!
//! Rules decide which scene a foreground executable selects. Matching is
//! case-insensitive and supports a single trailing or embedded `*`, which is
//! enough for `DeltaForce*.exe` and keeps the matcher reviewable.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::error::{HubError, Result};
use crate::value::CapabilityValue;

/// Only version 1 exists. Anything else is rejected instead of guessed at:
/// silently accepting a newer file would let a newer panel write settings an
/// older hubd misreads.
pub const SCENES_VERSION: u32 = 1;

fn default_version() -> u32 {
    SCENES_VERSION
}

/// One change a scene asks for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneItem {
    /// Stable capability type, for example `display.brightness`.
    pub cap_type: String,
    /// Optional `adapter:token` device id. Absent means "every device that
    /// advertises this capability", which is what a single-monitor scene
    /// wants and what makes scenes survive a monitor being swapped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<String>,
    pub value: CapabilityValue,
}

/// A named set of changes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Scene {
    pub name: String,
    #[serde(default)]
    pub items: Vec<SceneItem>,
}

/// Which scene a foreground executable selects.
///
/// Exactly one of `exe` and `default` must be set; a rule carrying neither is
/// a typo and is rejected rather than silently ignored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SceneRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exe: Option<String>,
    #[serde(default)]
    pub default: bool,
    pub scene: String,
    #[serde(default)]
    pub priority: i32,
}

/// Engine-wide switches. Both default to the conservative choice: never move a
/// display mode on its own, and never write anything at startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SceneOptions {
    #[serde(default)]
    pub allow_side_effects: bool,
    #[serde(default)]
    pub startup_apply: bool,
}

/// The whole `scenes.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenesFile {
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub scenes: BTreeMap<String, Scene>,
    #[serde(default)]
    pub rules: Vec<SceneRule>,
    #[serde(default)]
    pub options: SceneOptions,
}

/// What one `apply` actually did. Shown in the panel and returned by the RPC,
/// so a scene that silently stopped matching (a capability disappeared, a
/// monitor was ignored) is visible instead of looking like it worked.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReport {
    pub scene: String,
    /// Values that differed and were written.
    pub written: usize,
    /// Values the device already had: not a single byte was sent.
    pub unchanged: usize,
    /// Items left alone because they move the display mode and
    /// `allow_side_effects` is off.
    pub skipped: usize,
    /// Item-level failures, already formatted for display.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<String>,
    /// Items that named no device providing them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
}

impl Default for ScenesFile {
    fn default() -> Self {
        Self {
            version: SCENES_VERSION,
            scenes: BTreeMap::new(),
            rules: Vec::new(),
            options: SceneOptions::default(),
        }
    }
}

impl ScenesFile {
    /// An empty document that is still valid: no scenes, no rules, conservative
    /// options. This is what a fresh install starts from.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn scene(&self, id: &str) -> Option<&Scene> {
        self.scenes.get(id)
    }

    /// Reject documents the engine cannot honour. Every error names the field
    /// so a hand-edited file can be fixed without reading the source.
    pub fn validate(&self) -> Result<()> {
        if self.version != SCENES_VERSION {
            return Err(HubError::invalid_value(format!(
                "scenes file version {} is not supported (this build reads version {SCENES_VERSION})",
                self.version
            )));
        }
        for (id, scene) in &self.scenes {
            if id.trim().is_empty() {
                return Err(HubError::invalid_value("scene id must not be empty"));
            }
            if scene.name.trim().is_empty() {
                return Err(HubError::invalid_value(format!(
                    "scene `{id}` has an empty name"
                )));
            }
            for item in &scene.items {
                if item.cap_type.trim().is_empty() {
                    return Err(HubError::invalid_value(format!(
                        "scene `{id}` has an item with an empty cap_type"
                    )));
                }
                if let Some(device) = &item.device {
                    if device.trim().is_empty() {
                        return Err(HubError::invalid_value(format!(
                            "scene `{id}` pins capability `{}` to an empty device id",
                            item.cap_type
                        )));
                    }
                }
            }
        }
        for (index, rule) in self.rules.iter().enumerate() {
            match (&rule.exe, rule.default) {
                (Some(exe), false) => {
                    if exe.trim().is_empty() {
                        return Err(HubError::invalid_value(format!(
                            "rule {index} has an empty exe"
                        )));
                    }
                }
                (None, true) => {}
                (Some(_), true) => {
                    return Err(HubError::invalid_value(format!(
                        "rule {index} sets both exe and default"
                    )))
                }
                (None, false) => {
                    return Err(HubError::invalid_value(format!(
                        "rule {index} sets neither exe nor default"
                    )))
                }
            }
            if !self.scenes.contains_key(&rule.scene) {
                return Err(HubError::invalid_value(format!(
                    "rule {index} points at scene `{}`, which does not exist",
                    rule.scene
                )));
            }
        }
        Ok(())
    }

    /// Pick the scene for a foreground executable.
    ///
    /// Exe rules win over the default rule; among matching exe rules the
    /// highest priority wins and ties keep document order. Returning `None`
    /// means "no rule matched", which the engine treats as "leave everything
    /// alone" rather than as an error.
    pub fn match_rule(&self, exe: &str) -> Option<&str> {
        let exe = exe.trim();
        if exe.is_empty() {
            return None;
        }
        let mut best: Option<(&SceneRule, usize)> = None;
        let mut fallback: Option<(&SceneRule, usize)> = None;
        for (index, rule) in self.rules.iter().enumerate() {
            match &rule.exe {
                Some(pattern) if glob_match(pattern, exe) => {
                    let better = match best {
                        None => true,
                        Some((current, _)) => rule.priority > current.priority,
                    };
                    if better {
                        best = Some((rule, index));
                    }
                }
                Some(_) => {}
                // First default rule wins; later ones are dead weight but
                // not an error, so a user can reorder rules without editing.
                None if rule.default && fallback.is_none() => {
                    fallback = Some((rule, index));
                }
                None => {}
            }
        }
        best.or(fallback).map(|(rule, _)| rule.scene.as_str())
    }
}

/// Case-insensitive glob with `*` as the only wildcard.
///
/// Deliberately not a regex: an exe pattern is a file name, and letting users
/// write `(` or `\` into it would invite surprises. Matching is over the whole
/// string, so `chrome` does not match `chrome.exe`.
fn glob_match(pattern: &str, value: &str) -> bool {
    let pattern = pattern.to_ascii_lowercase();
    let value = value.to_ascii_lowercase();
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == value;
    }
    let mut rest = value.as_str();
    let starts_with_star = pattern.starts_with('*');
    let ends_with_star = pattern.ends_with('*');
    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && !starts_with_star {
            let Some(tail) = rest.strip_prefix(part) else {
                return false;
            };
            rest = tail;
            continue;
        }
        let Some(position) = rest.find(part) else {
            return false;
        };
        rest = &rest[position + part.len()..];
    }
    ends_with_star || rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(cap_type: &str, value: i64) -> SceneItem {
        SceneItem {
            cap_type: cap_type.to_string(),
            device: None,
            value: CapabilityValue::Int(value),
        }
    }

    fn sample() -> ScenesFile {
        let mut scenes = BTreeMap::new();
        scenes.insert(
            "daily".to_string(),
            Scene {
                name: "Daily".to_string(),
                items: vec![item("display.brightness", 50)],
            },
        );
        scenes.insert(
            "game".to_string(),
            Scene {
                name: "Game".to_string(),
                items: vec![item("display.brightness", 45)],
            },
        );
        ScenesFile {
            version: SCENES_VERSION,
            scenes,
            rules: vec![
                SceneRule {
                    exe: Some("cs2.exe".to_string()),
                    default: false,
                    scene: "game".to_string(),
                    priority: 10,
                },
                SceneRule {
                    exe: Some("DeltaForce*.exe".to_string()),
                    default: false,
                    scene: "game".to_string(),
                    priority: 10,
                },
                SceneRule {
                    exe: None,
                    default: true,
                    scene: "daily".to_string(),
                    priority: 0,
                },
            ],
            options: SceneOptions::default(),
        }
    }

    #[test]
    fn parses_a_document_and_defaults_options() {
        let json = r#"{
            "version": 1,
            "scenes": { "s1": { "name": "Daily", "items": [
                { "cap_type": "display.brightness", "value": { "type": "int", "value": 50 } }
            ] } },
            "rules": [ { "default": true, "scene": "s1" } ]
        }"#;
        let file: ScenesFile = serde_json::from_str(json).unwrap();
        assert_eq!(file.options, SceneOptions::default());
        assert!(!file.options.allow_side_effects);
        assert_eq!(file.rules[0].priority, 0);
        file.validate().unwrap();
    }

    #[test]
    fn rejects_a_rule_pointing_at_a_missing_scene() {
        let mut file = sample();
        file.rules[0].scene = "nope".to_string();
        let error = file.validate().unwrap_err();
        assert!(error.to_string().contains("nope"), "{error}");
    }

    #[test]
    fn rejects_a_rule_with_neither_exe_nor_default() {
        let mut file = sample();
        file.rules[0].exe = None;
        file.rules[0].default = false;
        assert!(file.validate().is_err());
    }

    #[test]
    fn matches_exact_glob_and_default() {
        let file = sample();
        assert_eq!(file.match_rule("cs2.exe"), Some("game"));
        assert_eq!(file.match_rule("CS2.EXE"), Some("game"));
        assert_eq!(file.match_rule("DeltaForceClient.exe"), Some("game"));
        assert_eq!(file.match_rule("explorer.exe"), Some("daily"));
        // `chrome` must not match `chrome.exe`: a pattern without `*` is exact.
        assert_eq!(file.match_rule("chrome"), Some("daily"));
    }

    #[test]
    fn higher_priority_wins_over_document_order() {
        let mut file = sample();
        file.rules.insert(
            0,
            SceneRule {
                exe: Some("cs2.exe".to_string()),
                default: false,
                scene: "daily".to_string(),
                priority: 99,
            },
        );
        assert_eq!(file.match_rule("cs2.exe"), Some("daily"));
    }

    #[test]
    fn no_rules_means_no_scene() {
        let file = ScenesFile::empty();
        assert_eq!(file.match_rule("cs2.exe"), None);
    }

    #[test]
    fn glob_edge_cases() {
        assert!(glob_match("*", "anything.exe"));
        assert!(glob_match("*.exe", "cs2.exe"));
        assert!(!glob_match("*.exe", "cs2.exe.bak"));
        assert!(glob_match("a*b*c", "axxbyyc"));
        assert!(glob_match("a*middle*z", "a1middle2z"));
        assert!(!glob_match("a*middle*z", "a1middle2"));
    }
}
