//! Mock adapter: two fake devices that behave like real hardware in the ways
//! that matter for the hub — every call takes time, writes are read back, and
//! one capability (battery) is read-only.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use hub_core::{
    Access, Adapter, AdapterFactory, AdapterInfo, AdapterKind, CapabilityDescriptor,
    CapabilityKind, CapabilityValue, Cost, DeviceDescriptor, EnumOption, HubError, Result, Rgb,
    SideEffect, Traits,
};

const DISPLAY: &str = "mock:display-1";
const KEYBOARD: &str = "mock:keyboard-1";
const LATENCY: Duration = Duration::from_millis(30);

pub fn adapter_info() -> AdapterInfo {
    AdapterInfo {
        id: "mock".into(),
        name: "Mock devices".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        kind: AdapterKind::InProcess,
    }
}

pub fn factory() -> AdapterFactory {
    Arc::new(|| Box::new(MockAdapter::new()))
}

pub struct MockAdapter {
    devices: Vec<DeviceDescriptor>,
    values: BTreeMap<String, CapabilityValue>,
}

impl Default for MockAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl MockAdapter {
    pub fn new() -> Self {
        let devices = vec![display(), keyboard()];
        let mut values = BTreeMap::new();
        values.insert(key(DISPLAY, "brightness"), CapabilityValue::Int(62));
        values.insert(key(DISPLAY, "contrast"), CapabilityValue::Int(50));
        values.insert(key(DISPLAY, "hdr"), CapabilityValue::Bool(false));
        values.insert(key(DISPLAY, "input"), CapabilityValue::Enum("dp1".into()));
        values.insert(
            key(KEYBOARD, "polling"),
            CapabilityValue::Enum("1000".into()),
        );
        values.insert(
            key(KEYBOARD, "lighting"),
            CapabilityValue::Rgb(Rgb::new(255, 122, 0)),
        );
        values.insert(key(KEYBOARD, "battery"), CapabilityValue::Int(76));
        Self { devices, values }
    }
}

impl Adapter for MockAdapter {
    fn info(&self) -> AdapterInfo {
        adapter_info()
    }

    fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        std::thread::sleep(LATENCY);
        Ok(self.devices.clone())
    }

    fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        std::thread::sleep(LATENCY);
        self.values
            .get(&key(device, capability))
            .cloned()
            .ok_or_else(|| {
                HubError::unknown_capability(format!(
                    "mock device `{device}` has no capability `{capability}`"
                ))
            })
    }

    fn write(&mut self, device: &str, capability: &str, value: &CapabilityValue) -> Result<()> {
        std::thread::sleep(LATENCY);
        let slot = self
            .values
            .get_mut(&key(device, capability))
            .ok_or_else(|| {
                HubError::unknown_capability(format!(
                    "mock device `{device}` has no capability `{capability}`"
                ))
            })?;
        *slot = value.clone();
        Ok(())
    }
}

fn key(device: &str, capability: &str) -> String {
    format!("{device}/{capability}")
}

fn base(id: &str, cap_type: &str, label: &str, kind: CapabilityKind) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.into(),
        cap_type: cap_type.into(),
        label: label.into(),
        kind,
        access: Access::ReadWrite,
        cost: Cost {
            latency_ms: LATENCY.as_millis() as u32,
            side_effects: Vec::new(),
        },
        traits: Traits::default(),
    }
}

fn display() -> DeviceDescriptor {
    let mut hdr = base("hdr", "display.hdr", "HDR", CapabilityKind::Bool);
    hdr.cost.latency_ms = 800;
    hdr.cost.side_effects.push(SideEffect::SignalRenegotiation);

    let mut input = base(
        "input",
        "display.input_source",
        "Input source",
        CapabilityKind::Enum {
            options: vec![
                EnumOption {
                    value: "dp1".into(),
                    label: "DisplayPort 1".into(),
                },
                EnumOption {
                    value: "hdmi1".into(),
                    label: "HDMI 1".into(),
                },
                EnumOption {
                    value: "hdmi2".into(),
                    label: "HDMI 2".into(),
                },
                EnumOption {
                    value: "usb-c".into(),
                    label: "USB-C".into(),
                },
            ],
        },
    );
    input.cost.latency_ms = 500;
    input
        .cost
        .side_effects
        .push(SideEffect::SignalRenegotiation);

    let brightness = base(
        "brightness",
        "display.brightness",
        "Brightness",
        CapabilityKind::Int {
            min: 0,
            max: 100,
            step: 1,
            unit: Some("%".into()),
        },
    );
    let contrast = base(
        "contrast",
        "display.contrast",
        "Contrast",
        CapabilityKind::Int {
            min: 0,
            max: 100,
            step: 1,
            unit: Some("%".into()),
        },
    );

    DeviceDescriptor {
        id: DISPLAY.into(),
        adapter: "mock".into(),
        name: "Mock 27\" QHD Display".into(),
        vendor: Some("Mock Labs".into()),
        model: Some("ML-27Q".into()),
        serial: Some("MOCK-DSP-0001".into()),
        capabilities: vec![brightness, contrast, hdr, input],
    }
}

fn keyboard() -> DeviceDescriptor {
    let polling = base(
        "polling",
        "keyboard.polling_rate",
        "Polling rate",
        CapabilityKind::Enum {
            options: vec![
                EnumOption {
                    value: "125".into(),
                    label: "125 Hz".into(),
                },
                EnumOption {
                    value: "500".into(),
                    label: "500 Hz".into(),
                },
                EnumOption {
                    value: "1000".into(),
                    label: "1000 Hz".into(),
                },
                EnumOption {
                    value: "2000".into(),
                    label: "2000 Hz".into(),
                },
            ],
        },
    );
    let lighting = base("lighting", "keyboard.rgb", "Lighting", CapabilityKind::Rgb);

    let mut battery = base(
        "battery",
        "battery.level",
        "Battery",
        CapabilityKind::Int {
            min: 0,
            max: 100,
            step: 1,
            unit: Some("%".into()),
        },
    );
    battery.access = Access::ReadOnly;
    battery.traits.sceneable = false;

    DeviceDescriptor {
        id: KEYBOARD.into(),
        adapter: "mock".into(),
        name: "Mock Mechanical Keyboard".into(),
        vendor: Some("Mock Labs".into()),
        model: Some("MK-87".into()),
        serial: Some("MOCK-KBD-0002".into()),
        capabilities: vec![polling, lighting, battery],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enumerates_two_valid_devices() {
        let mut adapter = MockAdapter::new();
        let devices = adapter.enumerate().unwrap();
        assert_eq!(devices.len(), 2);
        for device in &devices {
            device.validate().unwrap();
        }
    }

    #[test]
    fn writes_are_visible_on_read() {
        let mut adapter = MockAdapter::new();
        adapter
            .write(DISPLAY, "brightness", &CapabilityValue::Int(15))
            .unwrap();
        assert_eq!(
            adapter.read(DISPLAY, "brightness").unwrap(),
            CapabilityValue::Int(15)
        );
    }

    #[test]
    fn unknown_capability_is_a_typed_error() {
        let mut adapter = MockAdapter::new();
        let err = adapter.read(DISPLAY, "nope").unwrap_err();
        assert_eq!(err.kind, hub_core::ErrorKind::UnknownCapability);
    }
}
