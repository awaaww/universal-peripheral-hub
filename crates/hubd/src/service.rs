use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use hub_core::{CapabilityValue, Hub, HubError, Result, Settings, SettingsStore, WriteOutcome};
use hub_proto::{DevicesResult, HelloResult, ReadResult, SetBlockedParams, PROTOCOL_VERSION};
use tokio::sync::broadcast;

/// Events pushed to connected clients. `hubd` is the single owner of device
/// state; clients never poll for changes.
#[derive(Debug, Clone)]
pub enum Event {
    DevicesChanged(DevicesResult),
}

pub struct Service {
    hub: Mutex<Hub>,
    settings: Mutex<SettingsStore>,
    events: broadcast::Sender<Event>,
    started: Instant,
    ui: Option<PathBuf>,
}

/// Which built-in adapters to register.
///
/// Tests need a hermetic hub, so the DDC adapter — the only one that depends on
/// the machine it runs on — can be left out.
#[derive(Debug, Clone, Copy)]
pub struct ServiceOptions {
    pub ddc: bool,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        Self { ddc: true }
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
        Arc::new(Self {
            hub: Mutex::new(hub),
            settings: Mutex::new(store),
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
