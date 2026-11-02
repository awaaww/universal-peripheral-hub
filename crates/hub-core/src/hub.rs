use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use crate::adapter::{Adapter, AdapterFactory, AdapterInfo, AdapterKind};
use crate::device::DeviceDescriptor;
use crate::error::{ErrorKind, HubError, Result};
use crate::value::CapabilityValue;

struct Registered {
    info: AdapterInfo,
    factory: AdapterFactory,
}

struct Loaded {
    adapter: Box<dyn Adapter>,
    devices: Vec<DeviceDescriptor>,
}

/// Administrative view of an adapter, reported to the UI so a disabled adapter
/// can still be shown (with its last known device count) instead of vanishing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterState {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: AdapterKind,
    pub enabled: bool,
    pub loaded: bool,
    pub device_count: usize,
}

/// Result of a write: the value read back from the device (or the requested
/// value if the read-back itself failed) and whether it matched the request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteOutcome {
    pub value: CapabilityValue,
    pub matched: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readback_error: Option<String>,
}

/// Registry of adapters and the devices they currently report.
///
/// The hub owns adapter instances. Unloading an adapter drops the instance,
/// which is how device handles are released; loading it again constructs a new
/// instance from the factory.
#[derive(Default)]
pub struct Hub {
    registered: BTreeMap<String, Registered>,
    loaded: BTreeMap<String, Loaded>,
    disabled: BTreeSet<String>,
    revision: u64,
}

impl Hub {
    pub fn new() -> Self {
        Self::default()
    }

    /// Monotonic counter bumped whenever the set of visible devices changes.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn register(&mut self, info: AdapterInfo, factory: AdapterFactory) -> Result<()> {
        if info.id.trim().is_empty() {
            return Err(HubError::invalid_value("adapter id must not be empty"));
        }
        if self.registered.contains_key(&info.id) {
            return Err(HubError::invalid_value(format!(
                "adapter `{}` is already registered",
                info.id
            )));
        }
        self.registered
            .insert(info.id.clone(), Registered { info, factory });
        Ok(())
    }

    pub fn is_registered(&self, adapter: &str) -> bool {
        self.registered.contains_key(adapter)
    }

    pub fn is_loaded(&self, adapter: &str) -> bool {
        self.loaded.contains_key(adapter)
    }

    /// Construct the adapter and enumerate its devices. Idempotent: loading an
    /// already loaded adapter is a no-op.
    pub fn load(&mut self, adapter: &str) -> Result<()> {
        if self.loaded.contains_key(adapter) {
            return Ok(());
        }
        let registered = self.registered.get(adapter).ok_or_else(|| {
            HubError::new(
                ErrorKind::UnknownAdapter,
                format!("adapter `{adapter}` is not registered"),
            )
        })?;
        let mut instance = (registered.factory)();
        let mut devices = instance.enumerate()?;
        for device in &devices {
            if device.adapter != adapter {
                return Err(HubError::new(
                    ErrorKind::Protocol,
                    format!(
                        "adapter `{adapter}` reported device `{}` belonging to adapter `{}`",
                        device.id, device.adapter
                    ),
                ));
            }
            device.validate()?;
        }
        // Device ids are a global namespace; a collision would make read/write
        // ambiguous, so it is rejected rather than resolved by ordering.
        for other in self.loaded.values() {
            for existing in &other.devices {
                if let Some(duplicate) = devices.iter().find(|dev| dev.id == existing.id) {
                    return Err(HubError::new(
                        ErrorKind::Protocol,
                        format!(
                            "device id `{}` reported by `{adapter}` is already provided by another adapter",
                            duplicate.id
                        ),
                    ));
                }
            }
        }
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        self.loaded.insert(
            adapter.to_string(),
            Loaded {
                adapter: instance,
                devices,
            },
        );
        self.revision += 1;
        Ok(())
    }

    /// Drop the adapter instance and every device handle it owns.
    pub fn unload(&mut self, adapter: &str) -> Result<()> {
        if self.loaded.remove(adapter).is_some() {
            self.revision += 1;
        }
        Ok(())
    }

    pub fn set_enabled(&mut self, adapter: &str, enabled: bool) -> Result<()> {
        if !self.registered.contains_key(adapter) {
            return Err(HubError::new(
                ErrorKind::UnknownAdapter,
                format!("adapter `{adapter}` is not registered"),
            ));
        }
        if enabled {
            self.disabled.remove(adapter);
            self.load(adapter)
        } else {
            self.disabled.insert(adapter.to_string());
            self.unload(adapter)
        }
    }

    pub fn is_enabled(&self, adapter: &str) -> bool {
        !self.disabled.contains(adapter)
    }

    pub fn disabled_adapters(&self) -> Vec<String> {
        self.disabled.iter().cloned().collect()
    }

    /// Devices of all loaded adapters, ordered by id for stable UI rendering.
    pub fn devices(&self) -> Vec<DeviceDescriptor> {
        let mut devices: Vec<DeviceDescriptor> = self
            .loaded
            .values()
            .flat_map(|loaded| loaded.devices.iter().cloned())
            .collect();
        devices.sort_by(|a, b| a.id.cmp(&b.id));
        devices
    }

    pub fn adapters(&self) -> Vec<AdapterState> {
        self.registered
            .values()
            .map(|registered| {
                let loaded = self.loaded.get(&registered.info.id);
                AdapterState {
                    id: registered.info.id.clone(),
                    name: registered.info.name.clone(),
                    version: registered.info.version.clone(),
                    kind: registered.info.kind,
                    enabled: self.is_enabled(&registered.info.id),
                    loaded: loaded.is_some(),
                    device_count: loaded.map(|l| l.devices.len()).unwrap_or(0),
                }
            })
            .collect()
    }

    pub fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        let adapter_id = self.adapter_of(device)?;
        let cap = self
            .device(device)
            .and_then(|dev| dev.capability(capability))
            .cloned()
            .ok_or_else(|| {
                HubError::unknown_capability(format!(
                    "device `{device}` has no capability `{capability}`"
                ))
            })?;
        if !cap.access.can_read() {
            return Err(HubError::new(
                ErrorKind::ReadOnly,
                format!("capability `{capability}` of `{device}` is write-only"),
            ));
        }
        let loaded = self.loaded.get_mut(&adapter_id).expect("adapter is loaded");
        loaded.adapter.read(device, capability)
    }

    /// Write a capability, then read it back.
    ///
    /// The read-back is what the caller reports as the new state: devices
    /// quantise, clamp or ignore values, and pretending the request was applied
    /// is how a scene ends up out of sync with reality.
    pub fn write(
        &mut self,
        device: &str,
        capability: &str,
        value: &CapabilityValue,
    ) -> Result<WriteOutcome> {
        let adapter_id = self.adapter_of(device)?;
        let cap = self
            .device(device)
            .and_then(|dev| dev.capability(capability))
            .cloned()
            .ok_or_else(|| {
                HubError::unknown_capability(format!(
                    "device `{device}` has no capability `{capability}`"
                ))
            })?;
        if !cap.access.can_write() {
            return Err(HubError::new(
                ErrorKind::ReadOnly,
                format!("capability `{capability}` of `{device}` is read-only"),
            ));
        }
        value.conforms_to(capability, &cap.kind)?;

        {
            let loaded = self.loaded.get_mut(&adapter_id).expect("adapter is loaded");
            loaded.adapter.write(device, capability, value)?;
        }

        let loaded = self.loaded.get_mut(&adapter_id).expect("adapter is loaded");
        match loaded.adapter.read(device, capability) {
            Ok(actual) => Ok(WriteOutcome {
                matched: &actual == value,
                value: actual,
                readback_error: None,
            }),
            Err(err) => Ok(WriteOutcome {
                value: value.clone(),
                matched: false,
                readback_error: Some(err.to_string()),
            }),
        }
    }

    /// Adapter that owns `device`, or `UnknownDevice`.
    fn adapter_of(&self, device: &str) -> Result<String> {
        for (id, loaded) in &self.loaded {
            if loaded.devices.iter().any(|dev| dev.id == device) {
                return Ok(id.clone());
            }
        }
        Err(HubError::unknown_device(format!(
            "no loaded adapter owns `{device}`"
        )))
    }

    fn device(&self, device: &str) -> Option<&DeviceDescriptor> {
        self.loaded
            .values()
            .flat_map(|loaded| loaded.devices.iter())
            .find(|dev| dev.id == device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::AdapterKind;
    use crate::capability::{Access, CapabilityDescriptor, CapabilityKind, Cost, Traits};

    struct FakeAdapter {
        brightness: i64,
    }

    impl FakeAdapter {
        fn new() -> Self {
            Self { brightness: 50 }
        }

        fn info() -> AdapterInfo {
            AdapterInfo {
                id: "fake".into(),
                name: "Fake".into(),
                version: "1.0".into(),
                kind: AdapterKind::InProcess,
            }
        }

        fn device(&self) -> DeviceDescriptor {
            DeviceDescriptor {
                id: "fake:one".into(),
                adapter: "fake".into(),
                name: "Fake device".into(),
                vendor: None,
                model: None,
                serial: None,
                capabilities: vec![
                    CapabilityDescriptor {
                        id: "brightness".into(),
                        cap_type: "display.brightness".into(),
                        label: "Brightness".into(),
                        kind: CapabilityKind::Int {
                            min: 0,
                            max: 100,
                            step: 1,
                            unit: None,
                        },
                        access: Access::ReadWrite,
                        cost: Cost::default(),
                        traits: Traits::default(),
                    },
                    CapabilityDescriptor {
                        id: "battery".into(),
                        cap_type: "battery.level".into(),
                        label: "Battery".into(),
                        kind: CapabilityKind::Int {
                            min: 0,
                            max: 100,
                            step: 1,
                            unit: None,
                        },
                        access: Access::ReadOnly,
                        cost: Cost::default(),
                        traits: Traits::default(),
                    },
                ],
            }
        }
    }

    impl Adapter for FakeAdapter {
        fn info(&self) -> AdapterInfo {
            Self::info()
        }

        fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
            Ok(vec![self.device()])
        }

        fn read(&mut self, _device: &str, capability: &str) -> Result<CapabilityValue> {
            match capability {
                "brightness" => Ok(CapabilityValue::Int(self.brightness)),
                "battery" => Ok(CapabilityValue::Int(80)),
                other => Err(HubError::unknown_capability(other)),
            }
        }

        fn write(
            &mut self,
            _device: &str,
            capability: &str,
            value: &CapabilityValue,
        ) -> Result<()> {
            match (capability, value) {
                ("brightness", CapabilityValue::Int(v)) => {
                    self.brightness = *v;
                    Ok(())
                }
                _ => Err(HubError::invalid_value("unsupported write")),
            }
        }
    }

    fn hub() -> Hub {
        let mut hub = Hub::new();
        let factory: AdapterFactory = std::sync::Arc::new(|| Box::new(FakeAdapter::new()));
        hub.register(FakeAdapter::info(), factory).unwrap();
        hub
    }

    #[test]
    fn load_enumerates_and_unload_releases() {
        let mut hub = hub();
        assert!(hub.devices().is_empty());
        hub.load("fake").unwrap();
        assert_eq!(hub.devices().len(), 1);
        hub.unload("fake").unwrap();
        assert!(hub.devices().is_empty());
        assert!(!hub.is_loaded("fake"));
    }

    #[test]
    fn write_returns_readback_value() {
        let mut hub = hub();
        hub.load("fake").unwrap();
        let outcome = hub
            .write("fake:one", "brightness", &CapabilityValue::Int(70))
            .unwrap();
        assert_eq!(outcome.value, CapabilityValue::Int(70));
        assert!(outcome.matched);
        assert_eq!(
            hub.read("fake:one", "brightness").unwrap(),
            CapabilityValue::Int(70)
        );
    }

    #[test]
    fn read_only_capability_rejects_writes() {
        let mut hub = hub();
        hub.load("fake").unwrap();
        let err = hub
            .write("fake:one", "battery", &CapabilityValue::Int(10))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::ReadOnly);
    }

    #[test]
    fn out_of_range_value_is_rejected_before_the_adapter() {
        let mut hub = hub();
        hub.load("fake").unwrap();
        let err = hub
            .write("fake:one", "brightness", &CapabilityValue::Int(300))
            .unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidValue);
    }

    #[test]
    fn unknown_device_is_a_typed_error() {
        let mut hub = hub();
        let err = hub.read("nope", "brightness").unwrap_err();
        assert_eq!(err.kind, ErrorKind::UnknownDevice);
    }

    #[test]
    fn disable_unloads_and_enable_reloads() {
        let mut hub = hub();
        hub.load("fake").unwrap();
        hub.set_enabled("fake", false).unwrap();
        assert!(hub.devices().is_empty());
        assert!(!hub.is_enabled("fake"));
        hub.set_enabled("fake", true).unwrap();
        assert_eq!(hub.devices().len(), 1);
    }
}
