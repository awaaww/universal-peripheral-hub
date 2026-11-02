//! The DDC/CI adapter: policy over [`Transport`].
//!
//! Policy, learned from Exo, Twinkle Tray and our own KTC panel:
//!
//! * Enumeration is the only expensive step we do eagerly. Reading the MCCS
//!   capabilities string costs ~1.1 s on the KTC, so it happens once per monitor
//!   and is cached for the lifetime of the adapter.
//! * Reads retry twice; two consecutive failures for one VCP code stop us from
//!   asking again (negative cache). Without it, a monitor that reports a code
//!   in its capability string but rejects it costs two round trips per refresh.
//! * Writes settle before the hub reads back. The KTC needs ~120 ms; per-model
//!   configuration can raise this.
//! * Only one DDC request runs at a time. The hub already serialises adapter
//!   calls, and I2C is shared between outputs of the same GPU, so the adapter
//!   never issues concurrent requests itself.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use std::thread::sleep;
use std::time::Duration;

use hub_core::adapter::{Adapter, AdapterFactory, AdapterInfo, AdapterKind};
use hub_core::capability::{
    Access, CapabilityDescriptor, CapabilityKind, Cost, EnumOption, SideEffect, Traits,
};
use hub_core::device::DeviceDescriptor;
use hub_core::error::{HubError, Result};
use hub_core::value::CapabilityValue;

use crate::caps::{self, vcp, Capabilities};
use crate::config::{self, MonitorConfig};
use crate::transport::{RawMonitor, Transport, WinTransport};

pub const ADAPTER_ID: &str = "ddc";
const DEFAULT_SETTLE_MS: u64 = 120;
const READ_ATTEMPTS: u32 = 2;
const STRIKES_BEFORE_UNSUPPORTED: u32 = 2;

pub fn adapter_info() -> AdapterInfo {
    AdapterInfo {
        id: ADAPTER_ID.to_string(),
        name: "Displays (DDC/CI)".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: AdapterKind::InProcess,
    }
}

pub fn factory() -> AdapterFactory {
    std::sync::Arc::new(|| Box::new(DdcAdapter::new()))
}

pub struct DdcAdapter {
    core: Core<WinTransport>,
}

impl DdcAdapter {
    pub fn new() -> Self {
        Self {
            core: Core::new(
                WinTransport::new(),
                config::default_config_dir(),
                repo_data_dir(),
            ),
        }
    }
}

impl Default for DdcAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl Adapter for DdcAdapter {
    fn info(&self) -> AdapterInfo {
        adapter_info()
    }

    fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        self.core.enumerate()
    }

    fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        self.core.read(device, capability)
    }

    fn write(&mut self, device: &str, capability: &str, value: &CapabilityValue) -> Result<()> {
        self.core.write(device, capability, value)
    }
}

/// Bundled monitor data lives next to the source tree; installed builds may not
/// have it, and that is fine.
fn repo_data_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("UPH_DATA_DIR") {
        return Some(PathBuf::from(dir).join("monitors"));
    }
    let exe = std::env::current_exe().ok()?;
    for ancestor in exe.ancestors().skip(1).take(6) {
        let candidate = ancestor.join("data").join("monitors");
        if candidate.is_dir() {
            return Some(candidate);
        }
    }
    None
}

struct DeviceState {
    handle: isize,
    key: String,
    settle_ms: u64,
    /// VCP code used for brightness (per-model override, default 0x10).
    brightness_code: u8,
    /// `(min, max)` reported when the capability descriptor was built.
    ranges: BTreeMap<u8, (u32, u32)>,
    descriptor: DeviceDescriptor,
}

/// Transport-independent behaviour, unit-tested with a fake monitor.
pub struct Core<T: Transport> {
    transport: T,
    config_dir: PathBuf,
    data_dir: Option<PathBuf>,
    devices: BTreeMap<String, DeviceState>,
    capabilities: BTreeMap<String, Capabilities>,
    strikes: BTreeMap<(String, u8), u32>,
    unsupported: BTreeSet<(String, u8)>,
    blocked: Vec<String>,
}

impl<T: Transport> Core<T> {
    pub fn new(transport: T, config_dir: PathBuf, data_dir: Option<PathBuf>) -> Self {
        Self {
            transport,
            config_dir,
            data_dir,
            devices: BTreeMap::new(),
            capabilities: BTreeMap::new(),
            strikes: BTreeMap::new(),
            unsupported: BTreeSet::new(),
            blocked: Vec::new(),
        }
    }

    fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        self.blocked = config::blocked_keys(&self.config_dir.join("settings.json"));
        let raw = self
            .transport
            .monitors()
            .map_err(|error| HubError::new(hub_core::error::ErrorKind::Io, error.to_string()))?;

        let mut devices = BTreeMap::new();
        let mut seen_keys: BTreeMap<String, u32> = BTreeMap::new();

        for monitor in raw {
            if self.is_blocked(&monitor.key) {
                tracing::info!(key = %monitor.key, "monitor is blocked; skipping");
                continue;
            }

            let occurrences = seen_keys.entry(monitor.key.clone()).or_insert(0);
            *occurrences += 1;
            let suffix = if *occurrences > 1 {
                format!("-{}", occurrences)
            } else {
                String::new()
            };
            let device_id = format!("{ADAPTER_ID}:{}{suffix}", monitor.key.to_ascii_lowercase());

            match self.build_device(&monitor, device_id) {
                Ok(state) => {
                    devices.insert(state.descriptor.id.clone(), state);
                }
                Err(error) => {
                    // A single uncooperative monitor must not hide the others.
                    tracing::warn!(key = %monitor.key, %error, "cannot describe monitor");
                }
            }
        }

        let descriptors = devices
            .values()
            .map(|state| state.descriptor.clone())
            .collect();
        self.devices = devices;
        Ok(descriptors)
    }

    fn build_device(&mut self, monitor: &RawMonitor, device_id: String) -> Result<DeviceState> {
        let config = MonitorConfig::load(&monitor.key, &self.config_dir, self.data_dir.as_deref());
        let settle_ms = config.settle_ms.unwrap_or(DEFAULT_SETTLE_MS);
        let brightness_code = config.brightness_vcp.unwrap_or(vcp::BRIGHTNESS);

        let caps = self.capabilities_for(monitor);
        let unsupported: BTreeSet<u8> = config.quirks.unsupported_vcp.iter().copied().collect();

        let mut capabilities = Vec::new();
        let mut ranges = BTreeMap::new();

        // Brightness and contrast are integers; their maximum comes from the
        // monitor itself, so the UI shows the real range instead of 0..100.
        for (id, label, code) in [
            ("brightness", "Brightness", brightness_code),
            ("contrast", "Contrast", vcp::CONTRAST),
        ] {
            if unsupported.contains(&code) {
                continue;
            }
            if !caps.supports(code) && !caps.codes.is_empty() {
                continue;
            }
            match self.probe_range(monitor.handle, code) {
                Some((_, maximum)) => {
                    ranges.insert(code, (0, maximum));
                    capabilities.push(int_descriptor(id, label, maximum));
                }
                None => {
                    tracing::debug!(
                        key = %monitor.key,
                        code = format!("{code:#04X}"),
                        "monitor does not answer this VCP code"
                    );
                }
            }
        }

        if !unsupported.contains(&vcp::INPUT_SOURCE) && caps.supports(vcp::INPUT_SOURCE) {
            let values = caps.values(vcp::INPUT_SOURCE);
            let options: Vec<EnumOption> = values
                .iter()
                .map(|value| EnumOption {
                    value: format!("{value:02X}"),
                    label: caps::input_source_label(*value),
                })
                .collect();
            if !options.is_empty() {
                capabilities.push(CapabilityDescriptor {
                    id: "input_source".to_string(),
                    cap_type: "display.input_source".to_string(),
                    label: "Input source".to_string(),
                    kind: CapabilityKind::Enum { options },
                    access: Access::ReadWrite,
                    // Switching inputs renegotiates the link and may blank the
                    // panel for a second.
                    cost: Cost {
                        latency_ms: 500,
                        side_effects: vec![SideEffect::SignalRenegotiation],
                    },
                    traits: Traits::default(),
                });
            }
        }

        if !unsupported.contains(&vcp::HDR) && caps.supports(vcp::HDR) {
            capabilities.push(CapabilityDescriptor {
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
            });
        }

        let edid_name = monitor
            .identity
            .as_ref()
            .and_then(|identity| identity.monitor_name.clone());
        let display_name = caps.model.clone().or(edid_name);

        let (vendor, model, serial) = match &monitor.identity {
            Some(identity) => (
                identity.manufacturer.clone(),
                display_name.clone().unwrap_or_else(|| identity.key.clone()),
                format!("{:08X}", identity.serial),
            ),
            None => (
                "Unknown".to_string(),
                display_name.clone().unwrap_or_else(|| monitor.key.clone()),
                String::new(),
            ),
        };

        let name = display_name.unwrap_or_else(|| monitor.description.clone());

        let descriptor = DeviceDescriptor {
            id: device_id,
            adapter: ADAPTER_ID.to_string(),
            name,
            vendor: Some(vendor),
            model: Some(model),
            serial: Some(serial),
            capabilities,
        };
        descriptor
            .validate()
            .map_err(|error| HubError::internal(error.to_string()))?;

        Ok(DeviceState {
            handle: monitor.handle,
            key: monitor.key.clone(),
            settle_ms,
            brightness_code,
            ranges,
            descriptor,
        })
    }

    /// Capabilities string, read once per monitor key. Falls back to an empty
    /// set when the monitor refuses, in which case the fast VCP probes decide.
    fn capabilities_for(&mut self, monitor: &RawMonitor) -> Capabilities {
        if let Some(caps) = self.capabilities.get(&monitor.key) {
            return caps.clone();
        }
        let caps = match self.transport.capabilities(monitor.handle) {
            Ok(text) => Capabilities::parse(&text),
            Err(error) => {
                tracing::warn!(key = %monitor.key, %error, "cannot read capabilities; probing instead");
                Capabilities::default()
            }
        };
        self.capabilities.insert(monitor.key.clone(), caps.clone());
        caps
    }

    fn probe_range(&mut self, handle: isize, code: u8) -> Option<(u32, u32)> {
        let (current, maximum) = self.transport.read_vcp(handle, code).ok()?;
        if maximum == 0 {
            return None;
        }
        Some((current.min(maximum), maximum))
    }

    fn is_blocked(&self, key: &str) -> bool {
        self.blocked
            .iter()
            .any(|blocked| blocked.eq_ignore_ascii_case(key))
    }

    fn state(&self, device: &str) -> Result<&DeviceState> {
        self.devices
            .get(device)
            .ok_or_else(|| HubError::unknown_device(format!("`{device}` is not a DDC monitor")))
    }

    fn code_for(state: &DeviceState, capability: &str) -> Result<u8> {
        let descriptor = state
            .descriptor
            .capabilities
            .iter()
            .find(|cap| cap.id == capability)
            .ok_or_else(|| {
                HubError::unknown_capability(format!(
                    "`{capability}` is not exposed by {}",
                    state.descriptor.id
                ))
            })?;

        let code = match capability {
            "brightness" => state.brightness_code,
            "contrast" => vcp::CONTRAST,
            "input_source" => vcp::INPUT_SOURCE,
            "hdr" => vcp::HDR,
            other => {
                return Err(HubError::unsupported(format!(
                    "capability `{other}` has no VCP code"
                )))
            }
        };
        debug_assert!(descriptor.id == capability);
        Ok(code)
    }

    fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        let (handle, code, key, settle_ms) = {
            let state = self.state(device)?;
            (
                state.handle,
                Self::code_for(state, capability)?,
                state.key.clone(),
                state.settle_ms,
            )
        };
        let _ = settle_ms;

        if self.unsupported.contains(&(key.clone(), code)) {
            return Err(HubError::unsupported(format!(
                "{} no longer answers VCP {code:#04X}",
                device
            )));
        }

        let mut last = None;
        for attempt in 0..READ_ATTEMPTS {
            match self.transport.read_vcp(handle, code) {
                Ok((current, maximum)) => {
                    self.strikes.remove(&(key.clone(), code));
                    self.update_range(device, code, maximum);
                    return Ok(decode(capability, current));
                }
                Err(error) => {
                    last = Some(error);
                    if attempt + 1 < READ_ATTEMPTS {
                        sleep(Duration::from_millis(70));
                    }
                }
            }
        }

        let error = last.expect("at least one attempt");
        let strikes = self.strikes.entry((key.clone(), code)).or_insert(0);
        *strikes += 1;
        if *strikes >= STRIKES_BEFORE_UNSUPPORTED {
            self.unsupported.insert((key, code));
            tracing::info!(
                device,
                code = format!("{code:#04X}"),
                "marking VCP code unsupported after repeated failures"
            );
            return Err(HubError::unsupported(format!(
                "{} rejects VCP {code:#04X}: {error}",
                device
            )));
        }
        Err(classify(error.code, error.context))
    }

    fn update_range(&mut self, device: &str, code: u8, maximum: u32) {
        if maximum == 0 {
            return;
        }
        if let Some(state) = self.devices.get_mut(device) {
            let entry = state.ranges.entry(code).or_insert((0, maximum));
            if entry.1 != maximum {
                entry.1 = maximum;
            }
        }
    }

    fn write(&mut self, device: &str, capability: &str, value: &CapabilityValue) -> Result<()> {
        let (handle, code, key, settle_ms) = {
            let state = self.state(device)?;
            (
                state.handle,
                Self::code_for(state, capability)?,
                state.key.clone(),
                state.settle_ms,
            )
        };
        let _ = key;

        let descriptor = self
            .state(device)?
            .descriptor
            .capabilities
            .iter()
            .find(|cap| cap.id == capability)
            .cloned()
            .ok_or_else(|| HubError::unknown_capability(capability.to_string()))?;
        if !descriptor.access.can_write() {
            return Err(HubError::new(
                hub_core::error::ErrorKind::ReadOnly,
                format!("{device} exposes `{capability}` read-only"),
            ));
        }

        let raw = encode(value, &descriptor)?;
        self.transport
            .write_vcp(handle, code, raw)
            .map_err(|error| classify(error.code, error.context))?;

        // Let the panel latch the new value before the hub reads it back.
        sleep(Duration::from_millis(settle_ms));
        Ok(())
    }
}

fn int_descriptor(id: &str, label: &str, maximum: u32) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.to_string(),
        cap_type: format!("display.{id}"),
        label: label.to_string(),
        kind: CapabilityKind::Int {
            min: 0,
            max: i64::from(maximum.max(1)),
            step: 1,
            unit: Some("%".to_string()),
        },
        access: Access::ReadWrite,
        cost: Cost {
            latency_ms: 70,
            side_effects: Vec::new(),
        },
        traits: Traits::default(),
    }
}

fn decode(capability: &str, raw: u32) -> CapabilityValue {
    match capability {
        "brightness" | "contrast" => CapabilityValue::Int(i64::from(raw)),
        "hdr" => CapabilityValue::Bool(raw != 0),
        "input_source" => CapabilityValue::Enum(format!("{raw:02X}")),
        _ => CapabilityValue::Int(i64::from(raw)),
    }
}

fn encode(value: &CapabilityValue, descriptor: &CapabilityDescriptor) -> Result<u32> {
    match (value, &descriptor.kind) {
        (CapabilityValue::Int(number), CapabilityKind::Int { min, max, .. }) => {
            if *number < *min || *number > *max {
                return Err(HubError::invalid_value(format!(
                    "{} must be between {min} and {max}, got {number}",
                    descriptor.id
                )));
            }
            Ok(*number as u32)
        }
        (CapabilityValue::Bool(flag), CapabilityKind::Bool) => Ok(u32::from(*flag)),
        (CapabilityValue::Enum(selected), CapabilityKind::Enum { options }) => {
            let chosen = options
                .iter()
                .find(|option| option.value.eq_ignore_ascii_case(selected))
                .ok_or_else(|| {
                    HubError::invalid_value(format!(
                        "`{selected}` is not a valid {} value",
                        descriptor.id
                    ))
                })?;
            u32::from_str_radix(&chosen.value, 16)
                .map_err(|_| HubError::internal("enum option is not hexadecimal"))
        }
        _ => Err(HubError::invalid_value(format!(
            "value shape does not match capability `{}`",
            descriptor.id
        ))),
    }
}

/// Win32 error codes DDC surfaces through.
fn classify(code: u32, context: &'static str) -> HubError {
    use hub_core::error::ErrorKind;

    let kind = match code {
        5 => ErrorKind::PermissionDenied,
        21 => ErrorKind::Transient, // ERROR_NOT_READY
        31 => ErrorKind::Transient, // ERROR_GEN_FAILURE: the usual DDC failure
        1167 => ErrorKind::Io,      // ERROR_DEVICE_NOT_CONNECTED
        1460 => ErrorKind::Timeout,
        _ => ErrorKind::Transient,
    };
    HubError::new(kind, format!("{context} (Win32 {code})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::win::DdcError;

    struct FakeMonitor {
        caps: String,
        values: BTreeMap<u8, (u32, u32)>,
        fail_reads: BTreeSet<u8>,
        writes: Vec<(u8, u32)>,
    }

    struct FakeTransport {
        monitors: Vec<RawMonitor>,
        state: BTreeMap<isize, FakeMonitor>,
    }

    impl Transport for FakeTransport {
        fn monitors(&mut self) -> std::result::Result<Vec<RawMonitor>, DdcError> {
            Ok(self.monitors.clone())
        }

        fn read_vcp(
            &mut self,
            handle: isize,
            code: u8,
        ) -> std::result::Result<(u32, u32), DdcError> {
            let monitor = self.state.get_mut(&handle).expect("known handle");
            if monitor.fail_reads.contains(&code) {
                return Err(DdcError {
                    code: 31,
                    context: "fake",
                });
            }
            monitor.values.get(&code).copied().ok_or(DdcError {
                code: 31,
                context: "fake",
            })
        }

        fn write_vcp(
            &mut self,
            handle: isize,
            code: u8,
            value: u32,
        ) -> std::result::Result<(), DdcError> {
            let monitor = self.state.get_mut(&handle).expect("known handle");
            monitor.writes.push((code, value));
            if let Some(entry) = monitor.values.get_mut(&code) {
                entry.0 = value;
            }
            Ok(())
        }

        fn capabilities(&mut self, handle: isize) -> std::result::Result<String, DdcError> {
            Ok(self.state[&handle].caps.clone())
        }
    }

    fn adapter() -> Core<FakeTransport> {
        let mut values = BTreeMap::new();
        values.insert(vcp::BRIGHTNESS, (40, 100));
        values.insert(vcp::CONTRAST, (50, 100));
        values.insert(vcp::HDR, (0, 1));

        let transport = FakeTransport {
            monitors: vec![RawMonitor {
                key: "SKG2725".to_string(),
                weak: false,
                identity: None,
                description: "Generic PnP Monitor".to_string(),
                handle: 1,
            }],
            state: BTreeMap::from([(
                1,
                FakeMonitor {
                    caps: "(prot(monitor)vcp(10 12 60(11 12 0F) E9))".to_string(),
                    values,
                    fail_reads: BTreeSet::new(),
                    writes: Vec::new(),
                },
            )]),
        };
        Core::new(transport, std::env::temp_dir().join("uph-test-none"), None)
    }

    #[test]
    fn builds_descriptors_from_capabilities() {
        let mut core = adapter();
        let devices = core.enumerate().expect("enumerate");
        assert_eq!(devices.len(), 1);
        let caps: Vec<&str> = devices[0]
            .capabilities
            .iter()
            .map(|cap| cap.id.as_str())
            .collect();
        assert_eq!(caps, vec!["brightness", "contrast", "input_source", "hdr"]);
        assert_eq!(devices[0].id, "ddc:skg2725");

        let brightness = &devices[0].capabilities[0];
        assert!(matches!(
            brightness.kind,
            CapabilityKind::Int { max: 100, .. }
        ));
        let hdr = &devices[0].capabilities[3];
        assert!(hdr.is_renegotiation());
    }

    #[test]
    fn reads_writes_and_encodes_values() {
        let mut core = adapter();
        core.enumerate().expect("enumerate");

        assert_eq!(
            core.read("ddc:skg2725", "brightness").unwrap(),
            CapabilityValue::Int(40)
        );
        core.write("ddc:skg2725", "brightness", &CapabilityValue::Int(65))
            .unwrap();
        assert_eq!(
            core.read("ddc:skg2725", "brightness").unwrap(),
            CapabilityValue::Int(65)
        );

        core.write("ddc:skg2725", "hdr", &CapabilityValue::Bool(true))
            .unwrap();
        assert_eq!(
            core.read("ddc:skg2725", "hdr").unwrap(),
            CapabilityValue::Bool(true)
        );
    }

    #[test]
    fn rejects_out_of_range_and_wrong_shape() {
        let mut core = adapter();
        core.enumerate().expect("enumerate");
        assert!(core
            .write("ddc:skg2725", "brightness", &CapabilityValue::Int(101))
            .is_err());
        assert!(core
            .write("ddc:skg2725", "brightness", &CapabilityValue::Bool(true))
            .is_err());
        assert!(core.read("ddc:skg2725", "nope").is_err());
        assert!(core.read("ddc:missing", "brightness").is_err());
    }

    #[test]
    fn negative_cache_after_two_failures() {
        let mut core = adapter();
        core.enumerate().expect("enumerate");

        {
            let monitor = core.transport.state.get_mut(&1).unwrap();
            monitor.fail_reads.insert(vcp::CONTRAST);
        }

        let first = core.read("ddc:skg2725", "contrast").unwrap_err();
        assert!(first.to_string().contains("Win32 31"), "{first}");
        let second = core.read("ddc:skg2725", "contrast").unwrap_err();
        assert_eq!(second.kind, hub_core::error::ErrorKind::Unsupported);

        // Third read is refused without touching the transport.
        let third = core.read("ddc:skg2725", "contrast").unwrap_err();
        assert_eq!(third.kind, hub_core::error::ErrorKind::Unsupported);
    }

    #[test]
    fn blocked_monitors_are_skipped() {
        let dir = std::env::temp_dir().join(format!("uph-ddc-blocked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("settings.json"), r#"{"ddc_blocked":["SKG2725"]}"#).unwrap();

        let mut values = BTreeMap::new();
        values.insert(vcp::BRIGHTNESS, (40, 100));
        let transport = FakeTransport {
            monitors: vec![RawMonitor {
                key: "SKG2725".to_string(),
                weak: false,
                identity: None,
                description: "blocked".to_string(),
                handle: 7,
            }],
            state: BTreeMap::from([(
                7,
                FakeMonitor {
                    caps: "(vcp(10))".to_string(),
                    values,
                    fail_reads: BTreeSet::new(),
                    writes: Vec::new(),
                },
            )]),
        };
        let mut core = Core::new(transport, dir.clone(), None);
        assert!(core.enumerate().unwrap().is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }
}
