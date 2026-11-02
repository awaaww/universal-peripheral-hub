//! The scene engine: decides *when* a scene applies and performs the writes.
//!
//! Three rules from `docs/architecture.md` are enforced here rather than left
//! to callers, because each one of them was learned the hard way on real
//! hardware:
//!
//! 1. At startup the engine only records a baseline. Writing a whole group of
//!    values on every login flashes the monitor black each time.
//! 2. Before writing, read. If the device already has the value, not a single
//!    byte is sent.
//! 3. Capabilities that make the display renegotiate its signal (HDR, picture
//!    mode, display application) are skipped unless `allow_side_effects` is on.
//!
//! The engine owns no threads and no locks. `hubd` runs it on one thread and
//! feeds it foreground changes, so `apply` can sleep between write retries
//! without stalling the RPC server.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hub_core::capability::CapabilityDescriptor;
use hub_core::device::DeviceDescriptor;
use hub_core::scene::{ApplyReport, SceneItem, ScenesFile};
use hub_core::value::CapabilityValue;
use hub_core::{ErrorKind, HubError, Result, WriteOutcome};
use tracing::{debug, info, warn};

/// Device access the engine needs. `hubd` implements this on top of `Hub`;
/// tests implement it with a table, so the ordering rules can be checked
/// without a monitor.
pub trait DeviceIo: Send {
    fn devices(&self) -> Vec<DeviceDescriptor>;
    fn read(&self, device: &str, capability: &str) -> Result<CapabilityValue>;
    fn write(
        &self,
        device: &str,
        capability: &str,
        value: &CapabilityValue,
    ) -> Result<WriteOutcome>;
}

/// How a document change reaches the devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocumentMode {
    /// Rules changed: re-evaluate the current foreground executable now.
    Map,
    /// Only the scene contents changed: persist, do not touch devices. The
    /// next foreground change picks the new values up.
    Save,
}

/// What caused the most recent scene application.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppliedBy {
    /// The startup baseline; reported so the panel can explain it.
    Startup,
    /// A foreground change matched a rule.
    Rule,
    /// The user pressed Apply.
    Manual,
}

/// Read-only view of the engine for the panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Runtime {
    pub armed: bool,
    pub current_scene: Option<String>,
    pub current_exe: Option<String>,
    pub last_applied_ms: Option<u64>,
    pub last_report: Option<ApplyReport>,
    pub applied_by: Option<AppliedBy>,
}

pub struct SceneEngine<I: DeviceIo> {
    io: I,
    document: ScenesFile,
    armed: bool,
    current_scene: Option<String>,
    current_exe: Option<String>,
    last_applied_ms: Option<u64>,
    last_report: Option<ApplyReport>,
    applied_by: Option<AppliedBy>,
}

impl<I: DeviceIo> SceneEngine<I> {
    pub fn new(io: I, document: ScenesFile) -> Result<Self> {
        document.validate()?;
        Ok(Self {
            io,
            document,
            armed: false,
            current_scene: None,
            current_exe: None,
            last_applied_ms: None,
            last_report: None,
            applied_by: None,
        })
    }

    pub fn document(&self) -> &ScenesFile {
        &self.document
    }

    pub fn runtime(&self) -> Runtime {
        Runtime {
            armed: self.armed,
            current_scene: self.current_scene.clone(),
            current_exe: self.current_exe.clone(),
            last_applied_ms: self.last_applied_ms,
            last_report: self.last_report.clone(),
            applied_by: self.applied_by,
        }
    }

    /// Replace the document. `Map` re-evaluates the executable the user is in
    /// right now, so editing a rule and saving it takes effect immediately
    /// instead of on the next app switch.
    pub fn set_document(
        &mut self,
        document: ScenesFile,
        mode: DocumentMode,
    ) -> Result<Option<ApplyReport>> {
        document.validate()?;
        self.document = document;
        if mode != DocumentMode::Map {
            return Ok(None);
        }
        let Some(exe) = self.current_exe.clone() else {
            return Ok(None);
        };
        let scene = self.document.match_rule(&exe).map(str::to_owned);
        self.current_scene = scene.clone();
        match scene {
            Some(id) => self.apply(&id).map(Some),
            None => Ok(None),
        }
    }

    /// Feed the engine the executable that just became foreground.
    ///
    /// Returns the report when a scene was applied, and `None` when the call
    /// only moved the baseline or no rule matched.
    pub fn on_foreground(&mut self, exe: &str) -> Option<ApplyReport> {
        let exe = exe.trim().to_ascii_lowercase();
        if exe.is_empty() {
            return None;
        }
        if !self.armed {
            self.armed = true;
            self.current_exe = Some(exe.clone());
            self.current_scene = self.document.match_rule(&exe).map(str::to_owned);
            self.applied_by = Some(AppliedBy::Startup);
            info!(
                exe,
                scene = ?self.current_scene,
                startup_apply = self.document.options.startup_apply,
                "foreground baseline recorded"
            );
            if self.document.options.startup_apply {
                if let Some(scene) = self.current_scene.clone() {
                    return self.apply(&scene).ok();
                }
            }
            return None;
        }
        if self.current_exe.as_deref() == Some(exe.as_str()) {
            return None;
        }
        self.current_exe = Some(exe.clone());
        let scene = self.document.match_rule(&exe).map(str::to_owned);
        self.current_scene = scene.clone();
        let Some(id) = scene else {
            debug!(exe, "no rule matched; devices left alone");
            return None;
        };
        // Deliberately no "same scene as last time" shortcut: coming back to a
        // game must re-check the values, because something else (a monitor's
        // own menu, a previous scene) may have changed them in between.
        info!(exe, scene = %id, "applying scene for foreground change");
        self.applied_by = Some(AppliedBy::Rule);
        match self.apply(&id) {
            Ok(report) => Some(report),
            Err(error) => {
                warn!(scene = %id, %error, "scene could not be applied");
                Some(ApplyReport {
                    scene: id,
                    failed: vec![error.to_string()],
                    ..Default::default()
                })
            }
        }
    }

    /// Apply a scene because the user asked for it, not because of a rule.
    pub fn apply_now(&mut self, scene_id: &str) -> Result<ApplyReport> {
        self.applied_by = Some(AppliedBy::Manual);
        let report = self.apply(scene_id)?;
        self.current_scene = Some(scene_id.to_string());
        Ok(report)
    }

    fn apply(&mut self, scene_id: &str) -> Result<ApplyReport> {
        let Some(scene) = self.document.scene(scene_id) else {
            return Err(HubError::new(
                ErrorKind::InvalidValue,
                format!("scene `{scene_id}` does not exist"),
            ));
        };
        let devices = self.io.devices();
        let allow_side_effects = self.document.options.allow_side_effects;
        let mut report = ApplyReport {
            scene: scene_id.to_string(),
            ..Default::default()
        };

        struct Target<'a> {
            device: &'a str,
            capability: &'a CapabilityDescriptor,
            item: &'a SceneItem,
        }

        let mut targets: Vec<Target<'_>> = Vec::new();
        for item in &scene.items {
            let mut matched_device = false;
            for device in &devices {
                let Some(capability) = device
                    .capabilities
                    .iter()
                    .find(|capability| capability.cap_type == item.cap_type)
                else {
                    continue;
                };
                if item
                    .device
                    .as_deref()
                    .is_some_and(|pinned| pinned != device.id)
                {
                    continue;
                }
                matched_device = true;
                if !capability.access.can_write() {
                    report
                        .failed
                        .push(format!("{}: `{}` is read-only", device.id, item.cap_type));
                    continue;
                }
                targets.push(Target {
                    device: &device.id,
                    capability,
                    item,
                });
            }
            if !matched_device {
                report.missing.push(match &item.device {
                    Some(device) => format!("{} on {device}", item.cap_type),
                    None => format!("{} (no device provides it)", item.cap_type),
                });
            }
        }

        // Stable sort: renegotiation-class capabilities first, because they may
        // reset everything else the scene is about to set.
        targets.sort_by_key(|target| u8::from(target.capability.cost.side_effects.is_empty()));
        let total = targets.len();

        for target in targets {
            let name = format!("{} {}", target.device, target.item.cap_type);
            if let Err(error) = target
                .item
                .value
                .conforms_to(&target.capability.id, &target.capability.kind)
            {
                report.failed.push(format!("{name}: {error}"));
                continue;
            }
            let side_effect = !target.capability.cost.side_effects.is_empty();
            if side_effect && !allow_side_effects {
                report.skipped += 1;
                debug!(%name, "side effect skipped (allow_side_effects is off)");
                continue;
            }
            match self.io.read(target.device, &target.capability.id) {
                Ok(current) if current == target.item.value => {
                    report.unchanged += 1;
                    continue;
                }
                Ok(_) => {}
                // A failed read is not a reason to keep the old value: write
                // and let the hub's read-back decide whether it took.
                Err(error) => debug!(%name, %error, "read before write failed"),
            }
            let mut last_error = None;
            for attempt in 0..2 {
                match self
                    .io
                    .write(target.device, &target.capability.id, &target.item.value)
                {
                    Ok(_) => {
                        report.written += 1;
                        last_error = None;
                        break;
                    }
                    Err(error) => {
                        let retry = error.kind.retryable() && attempt == 0;
                        last_error = Some(error);
                        if !retry {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(50));
                    }
                }
            }
            if let Some(error) = last_error {
                warn!(%name, %error, "scene write failed");
                report.failed.push(format!("{name}: {error}"));
            }
        }

        info!(
            scene = scene_id,
            targets = total,
            written = report.written,
            unchanged = report.unchanged,
            skipped = report.skipped,
            failed = report.failed.len(),
            "scene applied"
        );
        self.last_applied_ms = Some(now_ms());
        self.last_report = Some(report.clone());
        Ok(report)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeMap;

    use hub_core::capability::{Access, CapabilityKind, Cost, SideEffect, Traits};
    use hub_core::device::DeviceDescriptor;
    use hub_core::scene::{Scene, SceneItem, SceneOptions, SceneRule};
    use hub_core::value::CapabilityValue;

    use super::*;

    #[derive(Default)]
    struct FakeIo {
        values: BTreeMap<(String, String), CapabilityValue>,
        writes: RefCell<Vec<(String, String, CapabilityValue)>>,
        fail_writes: bool,
    }

    impl DeviceIo for FakeIo {
        fn devices(&self) -> Vec<DeviceDescriptor> {
            vec![DeviceDescriptor {
                id: "fake:display".to_string(),
                adapter: "fake".to_string(),
                name: "Display".to_string(),
                vendor: None,
                model: None,
                serial: None,
                capabilities: vec![
                    CapabilityDescriptor {
                        id: "brightness".to_string(),
                        cap_type: "display.brightness".to_string(),
                        label: "Brightness".to_string(),
                        kind: CapabilityKind::Int {
                            min: 0,
                            max: 100,
                            step: 1,
                            unit: Some("%".to_string()),
                        },
                        access: Access::ReadWrite,
                        cost: Cost::default(),
                        traits: Traits::default(),
                    },
                    CapabilityDescriptor {
                        id: "hdr".to_string(),
                        cap_type: "display.hdr".to_string(),
                        label: "HDR".to_string(),
                        kind: CapabilityKind::Bool,
                        access: Access::ReadWrite,
                        cost: Cost {
                            latency_ms: 800,
                            side_effects: vec![SideEffect::SignalRenegotiation],
                        },
                        traits: Traits::default(),
                    },
                ],
            }]
        }

        fn read(&self, device: &str, capability: &str) -> Result<CapabilityValue> {
            self.values
                .get(&(device.to_string(), capability.to_string()))
                .cloned()
                .ok_or_else(|| HubError::unknown_capability(capability))
        }

        fn write(
            &self,
            device: &str,
            capability: &str,
            value: &CapabilityValue,
        ) -> Result<WriteOutcome> {
            if self.fail_writes {
                return Err(HubError::new(ErrorKind::Transient, "bus busy"));
            }
            self.writes.borrow_mut().push((
                device.to_string(),
                capability.to_string(),
                value.clone(),
            ));
            Ok(WriteOutcome {
                value: value.clone(),
                matched: true,
                readback_error: None,
            })
        }
    }

    fn display(value: i64) -> FakeIo {
        let mut io = FakeIo::default();
        io.values.insert(
            ("fake:display".to_string(), "brightness".to_string()),
            CapabilityValue::Int(value),
        );
        io.values.insert(
            ("fake:display".to_string(), "hdr".to_string()),
            CapabilityValue::Bool(false),
        );
        io
    }

    fn document(allow_side_effects: bool, brightness: i64) -> ScenesFile {
        let mut scenes = BTreeMap::new();
        scenes.insert(
            "game".to_string(),
            Scene {
                name: "Game".to_string(),
                items: vec![
                    SceneItem {
                        cap_type: "display.brightness".to_string(),
                        device: None,
                        value: CapabilityValue::Int(brightness),
                    },
                    SceneItem {
                        cap_type: "display.hdr".to_string(),
                        device: None,
                        value: CapabilityValue::Bool(true),
                    },
                ],
            },
        );
        ScenesFile {
            version: hub_core::SCENES_VERSION,
            scenes,
            rules: vec![SceneRule {
                exe: Some("cs2.exe".to_string()),
                default: false,
                scene: "game".to_string(),
                priority: 10,
            }],
            options: SceneOptions {
                allow_side_effects,
                startup_apply: false,
            },
        }
    }

    #[test]
    fn baseline_writes_nothing() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        assert!(engine.on_foreground("cs2.exe").is_none());
        assert!(engine.runtime().armed);
        assert_eq!(engine.runtime().current_scene.as_deref(), Some("game"));
    }

    #[test]
    fn reports_what_caused_the_current_scene() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        engine.on_foreground("explorer.exe");
        assert_eq!(engine.runtime().applied_by, Some(AppliedBy::Startup));
        engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(engine.runtime().applied_by, Some(AppliedBy::Rule));
        engine.apply_now("game").unwrap();
        assert_eq!(engine.runtime().applied_by, Some(AppliedBy::Manual));
    }

    #[test]
    fn re_entering_the_same_exe_does_not_rewrite() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        engine.on_foreground("cs2.exe");
        engine.on_foreground("cs2.exe");
        assert!(engine.io.writes.borrow().is_empty());
    }

    #[test]
    fn writes_only_what_differs_and_skips_side_effects_by_default() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        engine.on_foreground("explorer.exe"); // baseline
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.written, 1, "{report:?}");
        assert_eq!(report.skipped, 1, "{report:?}");
        let writes = engine.io.writes.borrow();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0].1, "brightness");
    }

    #[test]
    fn equal_value_is_not_written() {
        let mut engine = SceneEngine::new(display(45), document(false, 45)).unwrap();
        engine.on_foreground("explorer.exe");
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.written, 0, "{report:?}");
        assert_eq!(report.unchanged, 1, "{report:?}");
        assert!(engine.io.writes.borrow().is_empty());
    }

    #[test]
    fn side_effects_are_written_first_when_allowed() {
        let mut engine = SceneEngine::new(display(50), document(true, 45)).unwrap();
        engine.on_foreground("explorer.exe");
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.written, 2, "{report:?}");
        let writes = engine.io.writes.borrow();
        assert_eq!(writes[0].1, "hdr", "renegotiation-class first");
    }

    #[test]
    fn a_failed_write_is_reported_and_the_rest_continues() {
        let mut io = display(50);
        io.fail_writes = true;
        let mut engine = SceneEngine::new(io, document(true, 45)).unwrap();
        engine.on_foreground("explorer.exe");
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.written, 0);
        assert_eq!(report.failed.len(), 2);
    }

    #[test]
    fn map_mode_re_applies_for_the_current_exe() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        engine.on_foreground("cs2.exe");
        assert!(engine.io.writes.borrow().is_empty());
        let report = engine
            .set_document(document(false, 30), DocumentMode::Map)
            .unwrap()
            .unwrap();
        assert_eq!(report.written, 1);
        assert_eq!(engine.io.writes.borrow().len(), 1);
    }

    #[test]
    fn save_mode_does_not_touch_devices() {
        let mut engine = SceneEngine::new(display(50), document(false, 45)).unwrap();
        engine.on_foreground("cs2.exe");
        engine.on_foreground("explorer.exe");
        let before = engine.io.writes.borrow().len();
        assert!(engine
            .set_document(document(false, 30), DocumentMode::Save)
            .unwrap()
            .is_none());
        assert_eq!(engine.io.writes.borrow().len(), before);
    }

    #[test]
    fn an_item_no_device_provides_is_reported() {
        let mut doc = document(false, 45);
        doc.scenes.get_mut("game").unwrap().items.push(SceneItem {
            cap_type: "keyboard.polling_rate".to_string(),
            device: None,
            value: CapabilityValue::Enum("1000".to_string()),
        });
        let mut engine = SceneEngine::new(display(50), doc).unwrap();
        engine.on_foreground("explorer.exe");
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.missing.len(), 1, "{report:?}");
    }

    #[test]
    fn a_pinned_device_that_does_not_match_is_not_applied() {
        let mut doc = document(false, 45);
        doc.scenes.get_mut("game").unwrap().items[0].device = Some("other:display".to_string());
        let mut engine = SceneEngine::new(display(50), doc).unwrap();
        engine.on_foreground("explorer.exe");
        let report = engine.on_foreground("cs2.exe").unwrap();
        assert_eq!(report.missing.len(), 1, "{report:?}");
        assert_eq!(report.written, 0, "{report:?}");
    }
}
