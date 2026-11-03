use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use hub_core::device::DeviceDescriptor;
use hub_core::scene::ScenesFile;
use hub_core::{CapabilityValue, Hub, HubError, Result, Settings, SettingsStore, WriteOutcome};
use hub_proto::{
    ApplySceneParams, DevicesResult, HelloResult, ReadResult, ReloadParams, SceneMode,
    SceneRuntime, ScenesResult, SetBlockedParams, SetScenesParams, PROTOCOL_VERSION,
};
use tokio::sync::broadcast;

use crate::engine::{AppliedBy, DeviceIo, DocumentMode, SceneEngine};
use crate::proc;

/// Events pushed to connected clients. `hubd` is the single owner of device
/// state; clients never poll for changes.
#[derive(Debug, Clone)]
pub enum Event {
    DevicesChanged(DevicesResult),
    ScenesChanged(ScenesResult),
}

pub struct Service {
    hub: Arc<Mutex<Hub>>,
    settings: Mutex<SettingsStore>,
    scenes: Mutex<SceneEngine<HubIo>>,
    scenes_path: PathBuf,
    events: broadcast::Sender<Event>,
    started: Instant,
    ui: Option<PathBuf>,
}

/// Hub access for the scene engine. Each call takes the lock only for the
/// duration of one device operation, so a slow monitor does not block the
/// snapshot the panel is rendering.
struct HubIo {
    hub: Arc<Mutex<Hub>>,
}

impl DeviceIo for HubIo {
    fn devices(&self) -> Vec<DeviceDescriptor> {
        self.hub.lock().expect("hub mutex poisoned").devices()
    }

    fn read(&self, device: &str, capability: &str) -> Result<CapabilityValue> {
        self.hub
            .lock()
            .expect("hub mutex poisoned")
            .read(device, capability)
    }

    fn write(
        &self,
        device: &str,
        capability: &str,
        value: &CapabilityValue,
    ) -> Result<WriteOutcome> {
        self.hub
            .lock()
            .expect("hub mutex poisoned")
            .write(device, capability, value)
    }
}

/// Which built-in adapters to register.
///
/// Tests need a hermetic hub, so the adapters that depend on the machine they
/// run on can be left out: DDC talks to real monitors, ATK needs its helper
/// executable.
#[derive(Debug, Clone)]
pub struct ServiceOptions {
    pub ddc: bool,
    /// Register the out-of-process ATK mouse adapter when its executable is found.
    pub atk: bool,
    /// Explicit ATK helper path, overriding [`find_atk_adapter`]. Tests use this
    /// to point at a scripted executable (or at nothing).
    pub atk_binary: Option<PathBuf>,
    /// Register the out-of-process VIA keyboard adapter when its executable is found.
    pub via: bool,
    /// Explicit VIA helper path, overriding [`find_via_adapter`].
    pub via_binary: Option<PathBuf>,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self {
            ddc: true,
            atk: true,
            atk_binary: None,
            via: true,
            via_binary: None,
        }
    }
}

const ATK_BINARY_NAME: &str = "atk-adapter.exe";
const VIA_BINARY_NAME: &str = "via-adapter.exe";

/// Locate the ATK helper executable.
///
/// Same search order as [`crate::ui::find_ui_executable`]: an explicit
/// environment override, then a sibling of `hubd.exe` (installed layout), then
/// the usual development build directories relative to the executable and the
/// workspace root. A missing helper is not an error — the rest of the hub works
/// without it.
pub fn find_atk_adapter() -> Option<PathBuf> {
    find_helper("UPH_ATK_ADAPTER", ATK_BINARY_NAME, "atk")
}

/// Locate the VIA keyboard helper executable.
pub fn find_via_adapter() -> Option<PathBuf> {
    find_helper("UPH_VIA_ADAPTER", VIA_BINARY_NAME, "via")
}

/// Shared lookup for an out-of-process helper.
///
/// An explicit environment variable wins, then a sibling of `hubd.exe`, then the
/// usual development build directories. A missing helper is not an error: the
/// rest of the hub works without it.
pub fn find_helper(env_var: &str, binary_name: &str, adapter_dir: &str) -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os(env_var) {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
        tracing::warn!(
            path = %path.display(),
            env = env_var,
            "helper override does not point at a file; falling back to autodetection"
        );
    }

    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();

    for _ in 0..6 {
        for candidate in helper_candidates(&dir, binary_name, adapter_dir) {
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

fn helper_candidates(dir: &Path, binary_name: &str, adapter_dir: &str) -> Vec<PathBuf> {
    let mut found = vec![dir.join(binary_name)];
    for profile in ["debug", "release"] {
        found.push(dir.join("target").join(profile).join(binary_name));
        found.push(
            dir.join("adapters")
                .join(adapter_dir)
                .join("target")
                .join(profile)
                .join(binary_name),
        );
    }
    found.push(dir.join("bin").join(binary_name));
    found
}

/// One out-of-process helper the service may register.
struct HelperSpec {
    enabled: bool,
    explicit: Option<PathBuf>,
    found: Option<PathBuf>,
    id: &'static str,
    name: &'static str,
    env_var: &'static str,
}

impl HelperSpec {
    fn new(
        enabled: bool,
        explicit: Option<PathBuf>,
        found: Option<PathBuf>,
        id: &'static str,
        name: &'static str,
        env_var: &'static str,
    ) -> Self {
        Self {
            enabled,
            explicit,
            found,
            id,
            name,
            env_var,
        }
    }
}

impl Service {
    /// Build the service with every built-in adapter.
    pub fn bootstrap(settings_path: PathBuf, ui: Option<PathBuf>) -> Arc<Self> {
        Self::bootstrap_with(settings_path, ui, ServiceOptions::default())
    }

    /// Build the service from the settings file, register the requested
    /// adapters, honour the persisted disabled list and load everything else.
    pub fn bootstrap_with(
        settings_path: PathBuf,
        ui: Option<PathBuf>,
        options: ServiceOptions,
    ) -> Arc<Self> {
        let scenes_path = scenes_path_for(&settings_path);
        let store = SettingsStore::load(settings_path);
        if let Some(quarantined) = store.quarantined() {
            tracing::warn!(
                path = %quarantined.display(),
                "settings were unreadable and have been preserved"
            );
        }
        let disabled = store.settings().disabled_adapters.clone();

        let mut hub = Hub::new();
        hub.register(adapter_mock::adapter_info(), adapter_mock::factory())
            .expect("mock adapter id is a compile-time constant");

        // Real monitors. Enumeration is cheap (the capability string is only
        // read for monitors that answer); a machine without monitors simply
        // returns no devices.
        if options.ddc {
            if let Err(err) = hub.register(adapter_ddc::adapter_info(), adapter_ddc::factory()) {
                tracing::warn!(error = %err, "cannot register the DDC adapter");
            }
        }

        // Out of process, so a crash inside a helper cannot take the hub down
        // with it. A missing executable only means that adapter is not
        // registered; the hub still works without it.
        let helpers = [
            HelperSpec::new(
                options.atk,
                options.atk_binary.clone(),
                find_atk_adapter(),
                "atk",
                "ATK (HID)",
                "UPH_ATK_ADAPTER",
            ),
            HelperSpec::new(
                options.via,
                options.via_binary.clone(),
                find_via_adapter(),
                "via",
                "ATK RS7 Air (VIA)",
                "UPH_VIA_ADAPTER",
            ),
        ];
        for spec in helpers {
            if !spec.enabled {
                continue;
            }
            let HelperSpec {
                id, name, env_var, ..
            } = spec;
            match spec.explicit.or(spec.found) {
                Some(binary) => {
                    let info = proc::adapter_info_with(id, name);
                    let factory = proc::factory_for(info.clone(), binary.clone());
                    match hub.register(info, factory) {
                        Ok(()) => {
                            tracing::info!(adapter = id, path = %binary.display(), "adapter registered")
                        }
                        Err(err) => {
                            tracing::warn!(adapter = id, error = %err, "cannot register the adapter")
                        }
                    }
                }
                None => tracing::info!(
                    adapter = id,
                    env = env_var,
                    "no helper executable found; adapter is not registered"
                ),
            }
        }

        for adapter in &disabled {
            if hub.is_registered(adapter) {
                if let Err(err) = hub.set_enabled(adapter, false) {
                    tracing::warn!(adapter, error = %err, "cannot apply disabled state");
                }
            }
        }
        let to_load: Vec<String> = hub
            .adapters()
            .into_iter()
            .filter(|state| state.enabled)
            .map(|state| state.id)
            .collect();
        for adapter in to_load {
            if let Err(err) = hub.load(&adapter) {
                // A missing device must not prevent the hub from starting.
                tracing::warn!(adapter = %adapter, error = %err, "adapter failed to load");
            }
        }

        let (events, _) = broadcast::channel(32);
        let hub = Arc::new(Mutex::new(hub));
        let (document, quarantined) = load_scenes(&scenes_path);
        if let Some(path) = quarantined {
            tracing::warn!(
                path = %path.display(),
                "scenes file was unreadable and has been preserved"
            );
        }
        let engine = SceneEngine::new(
            HubIo {
                hub: Arc::clone(&hub),
            },
            document,
        )
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "scenes file rejected; starting without scenes");
            SceneEngine::new(
                HubIo {
                    hub: Arc::clone(&hub),
                },
                ScenesFile::empty(),
            )
            .expect("an empty document is always valid")
        });

        Arc::new(Self {
            hub,
            settings: Mutex::new(store),
            scenes: Mutex::new(engine),
            scenes_path,
            events,
            started: Instant::now(),
            ui,
        })
    }

    pub fn hello(&self) -> HelloResult {
        HelloResult {
            hub: "hubd".into(),
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: PROTOCOL_VERSION,
            pid: std::process::id(),
        }
    }

    pub fn uptime_ms(&self) -> u128 {
        self.started.elapsed().as_millis()
    }

    pub fn snapshot(&self) -> DevicesResult {
        let hub = self.hub.lock().expect("hub mutex poisoned");
        let settings = self.settings.lock().expect("settings mutex poisoned");
        devices_result(&hub, settings.settings())
    }

    pub fn read(&self, device: &str, capability: &str) -> Result<ReadResult> {
        let mut hub = self.hub.lock().expect("hub mutex poisoned");
        hub.read(device, capability)
            .map(|value| ReadResult { value })
    }

    pub fn write(
        &self,
        device: &str,
        capability: &str,
        value: &CapabilityValue,
    ) -> Result<WriteOutcome> {
        let mut hub = self.hub.lock().expect("hub mutex poisoned");
        let outcome = hub.write(device, capability, value)?;
        if outcome.readback_error.is_some() {
            tracing::warn!(
                device,
                capability,
                "write applied but read-back failed: {:?}",
                outcome.readback_error
            );
        } else if !outcome.matched {
            tracing::info!(
                device,
                capability,
                "device returned a different value than requested"
            );
        }
        Ok(outcome)
    }

    pub fn set_enabled(&self, adapter: &str, enabled: bool) -> Result<DevicesResult> {
        let (result, disabled) = {
            let mut hub = self.hub.lock().expect("hub mutex poisoned");
            if enabled {
                // A crashed adapter can stay *loaded* while its instance is
                // latched off, and `Hub::load` is a no-op for a loaded
                // adapter. Dropping the instance first gives the enable a
                // fresh process; the tray/panel re-enable path relies on it.
                hub.unload(adapter)?;
            }
            hub.set_enabled(adapter, enabled)?;
            let settings = self.settings.lock().expect("settings mutex poisoned");
            (
                devices_result(&hub, settings.settings()),
                hub.disabled_adapters(),
            )
        };

        {
            let mut settings = self.settings.lock().expect("settings mutex poisoned");
            settings.update(|settings| settings.disabled_adapters = disabled)?;
        }

        self.broadcast(Event::DevicesChanged(result.clone()));
        Ok(result)
    }

    /// Rebuild one adapter instance, clearing any latched failure state.
    ///
    /// `ProcAdapter` disables itself after three consecutive timeouts or when
    /// its child dies, and `Hub` stores adapter instances behind
    /// `Box<dyn Adapter>`, which cannot be downcast. Dropping the instance and
    /// letting the registered factory build a new one is therefore the reset
    /// path — the old instance's `Drop` kills and reaps its child process.
    ///
    /// No-op for an adapter the user has disabled: enabling it later already
    /// builds a fresh instance.
    pub fn reset_adapter(&self, adapter: &str) -> Result<DevicesResult> {
        let result = {
            let mut hub = self.hub.lock().expect("hub mutex poisoned");
            if !hub.is_registered(adapter) {
                return Err(HubError::new(
                    hub_core::ErrorKind::UnknownAdapter,
                    format!("unknown adapter `{adapter}`"),
                ));
            }
            hub.unload(adapter)?;
            if hub.is_enabled(adapter) {
                hub.load(adapter)?;
            }
            let settings = self.settings.lock().expect("settings mutex poisoned");
            devices_result(&hub, settings.settings())
        };

        self.broadcast(Event::DevicesChanged(result.clone()));
        Ok(result)
    }

    /// Re-enumerate adapters (all of them, or one).
    ///
    /// `Hub` caches the devices an adapter reported when it was loaded; a mouse
    /// that was asleep at that moment stays absent until the adapter is built
    /// again. The panel's Rescan button calls this instead of `devices.list`,
    /// because the latter only re-reads that cache.
    pub fn reload_adapters(&self, params: &ReloadParams) -> Result<DevicesResult> {
        let result = {
            let mut hub = self.hub.lock().expect("hub mutex poisoned");
            let ids: Vec<String> = match &params.adapter {
                Some(adapter) => {
                    if !hub.is_registered(adapter) {
                        return Err(HubError::new(
                            hub_core::ErrorKind::UnknownAdapter,
                            format!("unknown adapter `{adapter}`"),
                        ));
                    }
                    vec![adapter.clone()]
                }
                None => hub.adapters().into_iter().map(|state| state.id).collect(),
            };
            for id in ids {
                if !hub.is_enabled(&id) {
                    continue;
                }
                // One adapter failing to rebuild must not stop the others: a
                // vendor helper that is not installed is a normal state.
                if let Err(error) = hub.unload(&id) {
                    tracing::warn!(adapter = id, %error, "adapter could not be unloaded");
                    continue;
                }
                if let Err(error) = hub.load(&id) {
                    tracing::warn!(adapter = id, %error, "adapter could not be reloaded");
                }
            }
            let settings = self.settings.lock().expect("settings mutex poisoned");
            devices_result(&hub, settings.settings())
        };

        self.broadcast(Event::DevicesChanged(result.clone()));
        Ok(result)
    }

    /// Stop talking to one device, or resume.
    ///
    /// The block list is adapter-specific — the DDC adapter keys it by EDID so
    /// a monitor survives a replug on a different port — and it is applied when
    /// the adapter is built. Changing it therefore rebuilds that adapter: one
    /// re-enumeration instead of keeping a second, live-updated block list in
    /// two places.
    pub fn set_device_blocked(&self, params: &SetBlockedParams) -> Result<DevicesResult> {
        let (adapter, token) = params.target()?;
        let token = token.to_ascii_uppercase();

        let result = {
            let mut hub = self.hub.lock().expect("hub mutex poisoned");
            if !hub.is_registered(&adapter) {
                return Err(HubError::new(
                    hub_core::ErrorKind::UnknownAdapter,
                    format!("unknown adapter `{adapter}`"),
                ));
            }
            if adapter != "ddc" {
                return Err(HubError::unsupported(format!(
                    "adapter `{adapter}` has no block list"
                )));
            }

            let mut settings = self.settings.lock().expect("settings mutex poisoned");
            settings.update(|settings| {
                settings
                    .ddc_blocked
                    .retain(|key| !key.eq_ignore_ascii_case(&token));
                if params.blocked {
                    settings.ddc_blocked.push(token.clone());
                }
            })?;

            let enabled = hub
                .adapters()
                .iter()
                .any(|state| state.id == adapter && state.enabled);
            if enabled {
                hub.unload(&adapter)?;
                hub.load(&adapter)?;
            }
            devices_result(&hub, settings.settings())
        };

        self.broadcast(Event::DevicesChanged(result.clone()));
        Ok(result)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Event> {
        self.events.subscribe()
    }

    /// The scene document plus the engine's runtime view, for the panel.
    pub fn scenes(&self) -> ScenesResult {
        let engine = self.scenes.lock().expect("scenes mutex poisoned");
        scenes_result(&engine)
    }

    /// Persist a new document and, for `map` mode, recompute against the
    /// executable the user is in right now. Persisting happens before the
    /// in-memory swap: a failed save must not leave the running engine on a
    /// document that will disappear at the next restart.
    pub fn set_scenes(&self, params: &SetScenesParams) -> Result<ScenesResult> {
        params.document.validate()?;
        save_scenes(&self.scenes_path, &params.document)?;
        let mode = match params.mode {
            SceneMode::Map => DocumentMode::Map,
            SceneMode::Save => DocumentMode::Save,
        };
        let result = {
            let mut engine = self.scenes.lock().expect("scenes mutex poisoned");
            if let Some(report) = engine.set_document(params.document.clone(), mode)? {
                tracing::info!(
                    scene = %report.scene,
                    written = report.written,
                    unchanged = report.unchanged,
                    skipped = report.skipped,
                    failed = report.failed.len(),
                    "scene re-applied after a document change"
                );
            }
            scenes_result(&engine)
        };
        self.broadcast(Event::ScenesChanged(result.clone()));
        Ok(result)
    }

    /// Apply a scene because the user pressed a button in the panel.
    pub fn apply_scene(&self, params: &ApplySceneParams) -> Result<ScenesResult> {
        let result = {
            let mut engine = self.scenes.lock().expect("scenes mutex poisoned");
            let report = engine.apply_now(&params.scene)?;
            tracing::info!(
                scene = %report.scene,
                written = report.written,
                unchanged = report.unchanged,
                skipped = report.skipped,
                failed = report.failed.len(),
                missing = report.missing.len(),
                "scene applied on request"
            );
            scenes_result(&engine)
        };
        self.broadcast(Event::ScenesChanged(result.clone()));
        Ok(result)
    }

    /// Called from the foreground watcher thread. Does nothing when the
    /// executable did not change, so a chatty window switcher costs one lock.
    pub fn on_foreground(&self, exe: &str) {
        let (result, changed) = {
            let mut engine = self.scenes.lock().expect("scenes mutex poisoned");
            let before = engine.runtime();
            let applied = engine.on_foreground(exe).is_some();
            let after = engine.runtime();
            let changed = applied
                || before.current_scene != after.current_scene
                || before.armed != after.armed;
            (scenes_result(&engine), changed)
        };
        if changed {
            self.broadcast(Event::ScenesChanged(result));
        }
    }

    fn broadcast(&self, event: Event) {
        // A missing receiver only means no UI is connected.
        let _ = self.events.send(event);
    }

    /// Ask the UI to come up. Returns false when no UI executable was found,
    /// which is a configuration problem, not a device error.
    pub fn activate(&self) -> bool {
        let Some(path) = &self.ui else {
            tracing::warn!("no UI executable found; set UPH_UI_EXE to point at it");
            return false;
        };
        match launch(path) {
            Ok(()) => {
                tracing::info!(path = %path.display(), "launched UI");
                true
            }
            Err(err) => {
                tracing::warn!(path = %path.display(), error = %err, "cannot launch UI");
                false
            }
        }
    }

    pub fn ui_path(&self) -> Option<&std::path::Path> {
        self.ui.as_deref()
    }
}

fn scenes_result(engine: &SceneEngine<HubIo>) -> ScenesResult {
    let runtime = engine.runtime();
    ScenesResult {
        document: engine.document().clone(),
        runtime: SceneRuntime {
            armed: runtime.armed,
            current_scene: runtime.current_scene,
            current_exe: runtime.current_exe,
            last_applied_ms: runtime.last_applied_ms,
            last_report: runtime.last_report,
            last_source: runtime.applied_by.map(|by| match by {
                AppliedBy::Startup => "startup".to_string(),
                AppliedBy::Rule => "rule".to_string(),
                AppliedBy::Manual => "manual".to_string(),
            }),
        },
    }
}

fn scenes_path_for(settings_path: &Path) -> PathBuf {
    settings_path.with_file_name("scenes.json")
}

/// Read `scenes.json`, keeping an unreadable file instead of overwriting it.
///
/// A hand-edited file with a typo must be recoverable: the panel would happily
/// save an empty document over it and the user would lose their rules.
fn load_scenes(path: &Path) -> (ScenesFile, Option<PathBuf>) {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return (ScenesFile::empty(), None)
        }
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "cannot read scenes file");
            return (ScenesFile::empty(), None);
        }
    };
    let parsed = serde_json::from_str::<ScenesFile>(&text)
        .map_err(HubError::from)
        .and_then(|document| document.validate().map(|()| document));
    match parsed {
        Ok(document) => (document, None),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "scenes file rejected");
            let quarantined = quarantine(path);
            (ScenesFile::empty(), quarantined)
        }
    }
}

fn save_scenes(path: &Path, document: &ScenesFile) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut text = serde_json::to_string_pretty(document)?;
    text.push('\n');
    let temporary = path.with_extension("json.tmp");
    std::fs::write(&temporary, text)?;
    std::fs::rename(&temporary, path)?;
    Ok(())
}

/// Move a rejected file aside. Never delete: it is the only copy of the user's
/// rules, and a parser bug must not destroy data.
fn quarantine(path: &Path) -> Option<PathBuf> {
    let epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0);
    for attempt in 0..10 {
        let suffix = if attempt == 0 {
            format!(".corrupt-{epoch}")
        } else {
            format!(".corrupt-{epoch}-{attempt}")
        };
        let candidate = PathBuf::from(format!("{}{suffix}", path.display()));
        if candidate.exists() {
            continue;
        }
        return match std::fs::rename(path, &candidate) {
            Ok(()) => Some(candidate),
            Err(error) => {
                tracing::warn!(path = %path.display(), %error, "cannot quarantine scenes file");
                None
            }
        };
    }
    None
}

/// The snapshot every client sees: live device state plus the user's block
/// list, which lives in settings rather than in the adapter.
fn devices_result(hub: &Hub, settings: &Settings) -> DevicesResult {
    let mut blocked = BTreeMap::new();
    if !settings.ddc_blocked.is_empty() {
        blocked.insert("ddc".to_string(), settings.ddc_blocked.clone());
    }
    DevicesResult {
        devices: hub.devices(),
        adapters: hub.adapters(),
        revision: hub.revision(),
        blocked,
    }
}

fn launch(path: &std::path::Path) -> std::io::Result<()> {
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

    // ShellExecuteW rather than Command::new: this is a GUI program, and the
    // shell path gives it a normal foreground window. Command with
    // CREATE_NO_WINDOW starts it with a hidden show-state, which leaves the
    // panel minimised on some systems.
    let file = crate::winutil::to_wide(path.as_os_str());
    let code = unsafe {
        ShellExecuteW(
            0, // HWND is isize in windows-sys 0.52; 0 means "no owner window".
            std::ptr::null(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        ) as isize
    };
    if code <= 32 {
        return Err(std::io::Error::other(format!(
            "ShellExecuteW returned {code}"
        )));
    }
    Ok(())
}

/// Validate a write result against the requested value, used by `--selftest`
/// and by future scene code. Kept here so the rule lives in one place.
pub fn write_was_applied(outcome: &WriteOutcome) -> std::result::Result<(), HubError> {
    if outcome.readback_error.is_some() {
        return Err(HubError::new(
            hub_core::ErrorKind::Transient,
            "write applied but read-back failed",
        ));
    }
    if !outcome.matched {
        return Err(HubError::new(
            hub_core::ErrorKind::Protocol,
            "device did not accept the requested value",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("uph-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn corrupt_scenes_are_quarantined_not_lost() {
        let dir = scratch("scenes-corrupt");
        let path = dir.join("scenes.json");
        std::fs::write(&path, "{ this is not json").unwrap();

        let (document, quarantined) = load_scenes(&path);
        assert!(document.scenes.is_empty(), "fall back to an empty document");
        let quarantined = quarantined.expect("the unreadable file is preserved");
        assert_eq!(
            std::fs::read_to_string(&quarantined).unwrap(),
            "{ this is not json"
        );
        assert!(!path.exists(), "the broken file must not be overwritten");
    }

    #[test]
    fn semantically_invalid_scenes_are_quarantined_too() {
        let dir = scratch("scenes-invalid");
        let path = dir.join("scenes.json");
        std::fs::write(
            &path,
            r#"{"version":1,"scenes":{},"rules":[{"default":true,"scene":"ghost"}]}"#,
        )
        .unwrap();

        let (document, quarantined) = load_scenes(&path);
        assert!(document.scenes.is_empty());
        assert!(quarantined.is_some(), "a rule pointing nowhere is rejected");
    }

    #[test]
    fn scenes_survive_a_save_load_round_trip() {
        let dir = scratch("scenes-roundtrip");
        let path = dir.join("scenes.json");
        let mut document = ScenesFile::empty();
        document.scenes.insert(
            "work".to_string(),
            hub_core::Scene {
                name: "Work".to_string(),
                items: vec![hub_core::SceneItem {
                    cap_type: "display.brightness".to_string(),
                    device: None,
                    value: CapabilityValue::Int(30),
                }],
            },
        );
        document.rules.push(hub_core::SceneRule {
            exe: None,
            default: true,
            scene: "work".to_string(),
            priority: 0,
        });

        save_scenes(&path, &document).expect("save");
        let (loaded, quarantined) = load_scenes(&path);
        assert!(quarantined.is_none());
        assert_eq!(loaded, document);
    }

    #[test]
    fn a_missing_scenes_file_is_not_an_error() {
        let dir = scratch("scenes-missing");
        let (document, quarantined) = load_scenes(&dir.join("scenes.json"));
        assert!(document.scenes.is_empty());
        assert!(quarantined.is_none());
        assert_eq!(document.version, hub_core::SCENES_VERSION);
    }
}
