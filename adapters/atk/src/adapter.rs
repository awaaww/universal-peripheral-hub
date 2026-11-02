//! ATK adapter logic: discovery, descriptor generation and capability reads.
//!
//! Everything device-specific sits behind [`hid::Transport`], so the frame and
//! descriptor behaviour is exercised by a fake transport in tests.
//!
//! Honesty rules applied here:
//!
//! * a collection that does not answer the query protocol is logged and not
//!   exposed (the keyboard and headset use different command sets);
//! * only capabilities whose reply we actually decoded are advertised;
//! * writes are rejected as read-only until stage 3b.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use hub_core::{
    Access, Adapter, AdapterInfo, AdapterKind, CapabilityDescriptor, CapabilityKind,
    CapabilityValue, Cost, DeviceDescriptor, EnumOption, ErrorKind, HubError, Result, Traits,
};
use tracing::{debug, info, warn};

use crate::hid::{self, RawDevice, Transport, WindowsHid};
use crate::protocol;

pub const ADAPTER_ID: &str = "atk";
pub const ADAPTER_NAME: &str = "ATK (HID)";
/// Vendor control program that may hold the same collection open.
pub const ATK_HUB_EXE: &str = "ATK HUB.exe";
/// Replies arrive in milliseconds when the link is awake; the vendor app also
/// shows that an idle mouse can stay silent for a while.
const READ_TIMEOUT_MS: u32 = 800;
/// `OutputReportByteLength` of the mouse's vendor control collection (the
/// 17-byte collection verified on the 8K NANO dongle).
const MOUSE_CONTROL_REPORT_LEN: u16 = 17;
/// Honest per-query latency advertised to the hub.
const LATENCY_MS: u32 = 120;
/// `mouse.conflict` is a cheap process observation, not a device round trip.
const CONFLICT_LATENCY_MS: u32 = 0;

/// Capabilities a concrete collection answered during discovery.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Support {
    battery: bool,
    charging: bool,
    wireless_link: bool,
    firmware: bool,
    report_rate: bool,
    dpi_slot: bool,
}

impl Support {
    fn any(self) -> bool {
        self.battery
            || self.charging
            || self.wireless_link
            || self.firmware
            || self.report_rate
            || self.dpi_slot
    }
}

struct DeviceState {
    raw: RawDevice,
    descriptor: DeviceDescriptor,
    conflict: bool,
}

/// Adapter core generic over the HID transport.
pub struct Core<T: Transport> {
    transport: T,
    devices: BTreeMap<String, DeviceState>,
}

impl<T: Transport> Core<T> {
    pub fn new(transport: T) -> Self {
        Self {
            transport,
            devices: BTreeMap::new(),
        }
    }

    /// Enumerate, observing whether ATK HUB is running right now.
    pub fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        let atk_hub_running = hid::process_running(ATK_HUB_EXE);
        if atk_hub_running {
            warn!(
                process = ATK_HUB_EXE,
                "ATK HUB is running; it holds the same vendor collection open, so replies can race"
            );
        }
        self.enumerate_with(atk_hub_running)
    }

    /// Enumerate with an explicit conflict observation. Tests use this so they
    /// do not depend on what happens to be running on the machine.
    pub fn enumerate_with(&mut self, atk_hub_running: bool) -> Result<Vec<DeviceDescriptor>> {
        self.devices.clear();
        let mut candidates = self.transport.devices()?;
        // Deterministic order, and the smallest control collection of a device
        // is probed first: the 17-byte collection is the one that speaks the
        // query protocol.
        candidates.sort_by(|left, right| {
            left.key()
                .cmp(&right.key())
                .then(left.output_report_len.cmp(&right.output_report_len))
        });

        let mut exposed = Vec::new();
        for candidate in candidates {
            let id = format!("{ADAPTER_ID}:{}", candidate.key());
            if self.devices.contains_key(&id) {
                debug!(
                    id,
                    path = candidate.path,
                    usage_page = candidate.usage_page,
                    usage = candidate.usage,
                    "ATK: duplicate collection for an already exposed device"
                );
                continue;
            }
            match self.probe(&candidate) {
                Ok(support) if support.any() => {
                    let descriptor = describe(&id, &candidate, support, atk_hub_running);
                    if let Err(error) = descriptor.validate() {
                        warn!(id, %error, "ATK: generated an invalid descriptor");
                        continue;
                    }
                    self.devices.insert(
                        id,
                        DeviceState {
                            raw: candidate,
                            descriptor: descriptor.clone(),
                            conflict: atk_hub_running,
                        },
                    );
                    exposed.push(descriptor);
                }
                Ok(_) => {
                    info!(
                        path = candidate.path,
                        usage_page = candidate.usage_page,
                        "ATK: collection does not answer the ATK mouse query protocol; not exposing it"
                    );
                }
                Err(error)
                    if atk_hub_running
                        && candidate.output_report_len == MOUSE_CONTROL_REPORT_LEN
                        && error.kind.retryable() =>
                {
                    // The mouse control collection opened, but ATK HUB holds the
                    // same collection and the device stayed silent. No telemetry
                    // can be claimed, but the conflict itself is observable and
                    // is exactly what the user needs to see.
                    warn!(
                        path = candidate.path,
                        %error,
                        "ATK: mouse control collection is present but silent; exposing only the conflict flag"
                    );
                    let descriptor = describe(&id, &candidate, Support::default(), atk_hub_running);
                    if let Err(error) = descriptor.validate() {
                        warn!(id, %error, "ATK: generated an invalid descriptor");
                        continue;
                    }
                    self.devices.insert(
                        id,
                        DeviceState {
                            raw: candidate,
                            descriptor: descriptor.clone(),
                            conflict: atk_hub_running,
                        },
                    );
                    exposed.push(descriptor);
                }
                Err(error) => {
                    warn!(
                        path = candidate.path,
                        usage_page = candidate.usage_page,
                        %error,
                        "ATK: collection could not be probed"
                    );
                }
            }
        }
        Ok(exposed)
    }

    /// Ask the three plain queries and the EEPROM block. A silent first query
    /// means this collection does not speak the mouse protocol, so probing
    /// stops there instead of waiting three timeouts.
    fn probe(&mut self, device: &RawDevice) -> Result<Support> {
        self.transport.open(device)?;
        // A wireless mouse that has been idle stops answering until real input
        // arrives, and ATK HUB racing us for the collection looks the same.
        // Three quick attempts turn most of those into a normal enumerate
        // instead of a card that silently disappears until the adapter reloads.
        let mut last_error = None;
        let mut battery_reply = None;
        for attempt in 0..3 {
            match self.query(&protocol::query_battery()) {
                Ok(reply) => {
                    battery_reply = Some(reply);
                    break;
                }
                Err(error) => {
                    debug!(attempt, %error, "ATK: battery query did not answer");
                    last_error = Some(error);
                    std::thread::sleep(std::time::Duration::from_millis(150));
                }
            }
        }
        let battery_reply = match battery_reply {
            Some(reply) => reply,
            None => {
                return Err(last_error.unwrap_or_else(|| {
                    hub_core::HubError::new(hub_core::ErrorKind::Transient, "no battery reply")
                }))
            }
        };
        protocol::decode_battery(&battery_reply)?;
        let mut support = Support {
            battery: true,
            charging: true,
            ..Support::default()
        };

        match self.query(&protocol::query_wireless()) {
            Ok(reply) => match protocol::decode_wireless(&reply) {
                Ok(_) => support.wireless_link = true,
                Err(error) => debug!(%error, "ATK: wireless query answered with garbage"),
            },
            Err(error) => debug!(%error, "ATK: wireless query failed"),
        }

        match self.query(&protocol::query_version()) {
            Ok(reply) => match protocol::decode_version(&reply) {
                Ok(_) => support.firmware = true,
                Err(error) => debug!(%error, "ATK: version query answered with garbage"),
            },
            Err(error) => debug!(%error, "ATK: version query failed"),
        }

        match self.query(&protocol::query_eeprom(0, 10)) {
            Ok(reply) => match protocol::decode_eeprom(&reply) {
                Ok(settings) => {
                    support.report_rate =
                        protocol::report_rate_hz(settings.report_rate_mask).is_some();
                    support.dpi_slot = (1..=8).contains(&settings.current_dpi_slot);
                }
                Err(error) => debug!(%error, "ATK: EEPROM block answered with garbage"),
            },
            Err(error) => debug!(%error, "ATK: EEPROM query failed"),
        }

        Ok(support)
    }

    pub fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        let (raw, conflict) = {
            let state = self
                .devices
                .get(device)
                .ok_or_else(|| HubError::unknown_device(device.to_string()))?;
            if state.descriptor.capability(capability).is_none() {
                return Err(HubError::unknown_capability(format!(
                    "{device}: unknown capability `{capability}`"
                )));
            }
            (state.raw.clone(), state.conflict)
        };

        match capability {
            "battery" => {
                let reply = self.query_reply(&raw, &protocol::query_battery())?;
                Ok(CapabilityValue::Int(i64::from(
                    protocol::decode_battery(&reply)?.percent,
                )))
            }
            "charging" => {
                let reply = self.query_reply(&raw, &protocol::query_battery())?;
                Ok(CapabilityValue::Bool(
                    protocol::decode_battery(&reply)?.charging,
                ))
            }
            "wireless_link" => {
                let reply = self.query_reply(&raw, &protocol::query_wireless())?;
                Ok(CapabilityValue::Bool(protocol::decode_wireless(&reply)?))
            }
            "firmware" => {
                let reply = self.query_reply(&raw, &protocol::query_version())?;
                let (major, minor) = protocol::decode_version(&reply)?;
                Ok(CapabilityValue::Text(format!("{major}.{minor}")))
            }
            "report_rate" => {
                let settings = self.read_eeprom(&raw)?;
                let hertz =
                    protocol::report_rate_hz(settings.report_rate_mask).ok_or_else(|| {
                        HubError::new(
                            ErrorKind::Protocol,
                            format!(
                                "ATK: report-rate mask 0x{:02x} does not name one rate",
                                settings.report_rate_mask
                            ),
                        )
                    })?;
                Ok(CapabilityValue::Enum(hertz.to_string()))
            }
            "dpi_slot" => {
                let settings = self.read_eeprom(&raw)?;
                if !(1..=8).contains(&settings.current_dpi_slot) {
                    return Err(HubError::new(
                        ErrorKind::Protocol,
                        format!(
                            "ATK: current DPI slot {} is outside 1..=8",
                            settings.current_dpi_slot
                        ),
                    ));
                }
                Ok(CapabilityValue::Int(i64::from(settings.current_dpi_slot)))
            }
            "conflict" => Ok(CapabilityValue::Bool(conflict)),
            other => Err(HubError::unknown_capability(format!(
                "{device}: unknown capability `{other}`"
            ))),
        }
    }

    fn read_eeprom(&mut self, device: &RawDevice) -> Result<protocol::EepromSettings> {
        let reply = self.query_reply(device, &protocol::query_eeprom(0, 10))?;
        protocol::decode_eeprom(&reply)
    }

    /// One write + read, retrying once on a retryable failure. A sleeping mouse
    /// drops the first reply; the retry is the documented recovery.
    fn query_reply(&mut self, device: &RawDevice, frame: &[u8]) -> Result<Vec<u8>> {
        self.transport.open(device)?;
        match self.query(frame) {
            Ok(reply) => Ok(reply),
            Err(error) if error.kind.retryable() => {
                warn!(%error, "ATK: query failed with a retryable error; retrying once");
                self.query(frame)
            }
            Err(error) => Err(error),
        }
    }

    fn query(&mut self, frame: &[u8]) -> Result<Vec<u8>> {
        let expected = *frame
            .get(1)
            .ok_or_else(|| HubError::internal("ATK request frame has no opcode"))?;
        self.transport.write_frame(frame)?;

        // The vendor app keeps polling the same collection, and its replies are
        // delivered to every open handle. A frame carrying a different opcode
        // answers someone else's request; read past it instead of failing.
        let deadline = Instant::now() + Duration::from_millis(u64::from(READ_TIMEOUT_MS));
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HubError::new(
                    ErrorKind::Timeout,
                    format!(
                        "ATK HID: no reply to opcode 0x{expected:02x} within {READ_TIMEOUT_MS} ms"
                    ),
                ));
            }
            let reply = self
                .transport
                .read_frame(remaining.as_millis().max(1) as u32)?;
            let opcode = protocol::reply_opcode(&reply)?;
            if opcode == expected {
                return Ok(reply);
            }
            debug!(
                expected = format!("0x{expected:02x}"),
                got = format!("0x{opcode:02x}"),
                "ATK: discarding a reply for another command; the vendor app is polling the same collection"
            );
        }
    }

    pub fn write(
        &mut self,
        device: &str,
        capability: &str,
        _value: &CapabilityValue,
    ) -> Result<()> {
        let state = self
            .devices
            .get(device)
            .ok_or_else(|| HubError::unknown_device(device.to_string()))?;
        let descriptor = state.descriptor.capability(capability).ok_or_else(|| {
            HubError::unknown_capability(format!("{device}: unknown capability `{capability}`"))
        })?;
        if !descriptor.access.can_write() {
            return Err(HubError::new(
                ErrorKind::ReadOnly,
                format!("ATK device {device} exposes `{capability}` read-only"),
            ));
        }
        Err(HubError::unsupported(
            "ATK writes are not implemented in this milestone",
        ))
    }
}

/// Build the descriptor for one answering collection.
fn describe(
    id: &str,
    raw: &RawDevice,
    support: Support,
    atk_hub_running: bool,
) -> DeviceDescriptor {
    let product = raw
        .product
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let name = product
        .clone()
        .unwrap_or_else(|| format!("ATK device {}", raw.vendor_pid()));
    let mut capabilities = Vec::new();

    if support.battery {
        capabilities.push(descriptor(
            "battery",
            "mouse.battery",
            "Battery",
            CapabilityKind::Int {
                min: 0,
                max: 100,
                step: 1,
                unit: Some("%".to_string()),
            },
            LATENCY_MS,
        ));
    }
    if support.charging {
        capabilities.push(descriptor(
            "charging",
            "mouse.charging",
            "Charging",
            CapabilityKind::Bool,
            LATENCY_MS,
        ));
    }
    if support.wireless_link {
        capabilities.push(descriptor(
            "wireless_link",
            "mouse.wireless_link",
            "Wireless link",
            CapabilityKind::Bool,
            LATENCY_MS,
        ));
    }
    if support.firmware {
        capabilities.push(descriptor(
            "firmware",
            "mouse.firmware",
            "Firmware version",
            CapabilityKind::Text { max_len: 32 },
            LATENCY_MS,
        ));
    }
    if support.report_rate {
        let options = protocol::REPORT_RATES
            .iter()
            .map(|(_, hertz)| EnumOption {
                value: hertz.to_string(),
                label: format!("{hertz} Hz"),
            })
            .collect();
        capabilities.push(descriptor(
            "report_rate",
            "mouse.report_rate",
            "Report rate",
            CapabilityKind::Enum { options },
            LATENCY_MS,
        ));
    }
    if support.dpi_slot {
        // The firmware exposes a 1-based slot index; the slot values themselves
        // are still undecoded, so the real DPI is deliberately not advertised.
        capabilities.push(descriptor(
            "dpi_slot",
            "mouse.dpi_slot",
            "DPI slot",
            CapabilityKind::Int {
                min: 1,
                max: 8,
                step: 1,
                unit: None,
            },
            LATENCY_MS,
        ));
    }
    if atk_hub_running {
        capabilities.push(descriptor(
            "conflict",
            "mouse.conflict",
            "ATK HUB is running",
            CapabilityKind::Bool,
            CONFLICT_LATENCY_MS,
        ));
    }

    DeviceDescriptor {
        id: id.to_string(),
        adapter: ADAPTER_ID.to_string(),
        name,
        vendor: Some("ATK".to_string()),
        model: product,
        serial: raw.serial.clone(),
        capabilities,
    }
}

fn descriptor(
    id: &str,
    cap_type: &str,
    label: &str,
    kind: CapabilityKind,
    latency_ms: u32,
) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.to_string(),
        cap_type: cap_type.to_string(),
        label: label.to_string(),
        kind,
        access: Access::ReadOnly,
        cost: Cost {
            latency_ms,
            side_effects: Vec::new(),
        },
        traits: Traits::default(),
    }
}

/// Out-of-process adapter over the real HID transport.
pub struct AtkAdapter {
    core: Core<WindowsHid>,
}

impl Default for AtkAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl AtkAdapter {
    pub fn new() -> Self {
        Self {
            core: Core::new(WindowsHid::new()),
        }
    }
}

impl Adapter for AtkAdapter {
    fn info(&self) -> AdapterInfo {
        AdapterInfo {
            id: ADAPTER_ID.to_string(),
            name: ADAPTER_NAME.to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            kind: AdapterKind::OutOfProcess,
        }
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use hub_core::{Adapter, ErrorKind};

    use super::*;

    const BATTERY_REPLY: &[u8] = &[
        0x08, 0x04, 0x00, 0x00, 0x00, 0x02, 0x64, 0x00, 0x10, 0xE2, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0xF1,
    ];
    const WIRELESS_REPLY: &[u8] = &[
        0x08, 0x03, 0x00, 0x00, 0x00, 0x01, 0x01, 0x39, 0x3F, 0x58, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x78,
    ];
    const VERSION_REPLY: &[u8] = &[
        0x08, 0x12, 0x00, 0x00, 0x00, 0x02, 0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x33,
    ];
    const EEPROM_REPLY: &[u8] = &[
        0x08, 0x08, 0x00, 0x00, 0x00, 0x0A, 0x04, 0x51, 0x04, 0x51, 0x01, 0x54, 0x00, 0x55, 0x00,
        0x55, 0x92,
    ];

    #[derive(Default)]
    struct FakeTransport {
        devices: Vec<RawDevice>,
        replies: VecDeque<Result<Vec<u8>>>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(devices: Vec<RawDevice>, replies: Vec<Result<Vec<u8>>>) -> Self {
            Self {
                devices,
                replies: replies.into(),
                writes: Vec::new(),
            }
        }
    }

    impl Transport for FakeTransport {
        fn devices(&mut self) -> Result<Vec<RawDevice>> {
            Ok(self.devices.clone())
        }

        fn open(&mut self, _device: &RawDevice) -> Result<()> {
            Ok(())
        }

        fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
            self.writes.push(frame.to_vec());
            Ok(())
        }

        fn read_frame(&mut self, _timeout_ms: u32) -> Result<Vec<u8>> {
            self.replies
                .pop_front()
                .unwrap_or_else(|| Err(HubError::internal("fake transport ran out of replies")))
        }
    }

    fn mouse() -> RawDevice {
        RawDevice {
            path: "\\\\?\\hid#vid_373b&pid_11fe&mi_01&col05#test".to_string(),
            serial: Some("541505796617".to_string()),
            product: Some("Wireless mouse 8k NANO dongle-L".to_string()),
            vendor_id: 0x373B,
            product_id: 0x11FE,
            usage_page: 0xFF02,
            usage: 0x0002,
            input_report_len: 17,
            output_report_len: 17,
        }
    }

    fn probe_replies() -> Vec<Result<Vec<u8>>> {
        vec![
            Ok(BATTERY_REPLY.to_vec()),
            Ok(WIRELESS_REPLY.to_vec()),
            Ok(VERSION_REPLY.to_vec()),
            Ok(EEPROM_REPLY.to_vec()),
        ]
    }

    fn enumerated(conflict: bool) -> (Core<FakeTransport>, DeviceDescriptor) {
        let transport = FakeTransport::new(vec![mouse()], probe_replies());
        let mut core = Core::new(transport);
        let devices = core
            .enumerate_with(conflict)
            .expect("fake enumeration succeeds");
        assert_eq!(devices.len(), 1);
        (core, devices[0].clone())
    }

    #[test]
    fn device_id_is_the_lowercase_serial() {
        let (_, device) = enumerated(false);
        assert_eq!(device.id, "atk:541505796617");
        assert_eq!(device.adapter, "atk");
        assert_eq!(device.serial.as_deref(), Some("541505796617"));
        assert_eq!(device.name, "Wireless mouse 8k NANO dongle-L");
    }

    #[test]
    fn absent_serial_falls_back_to_vidpid() {
        let mut raw = mouse();
        raw.serial = None;
        let device = describe(
            "atk:373b:11fe",
            &raw,
            Support {
                battery: true,
                charging: true,
                ..Support::default()
            },
            false,
        );
        assert_eq!(raw.key(), "373b:11fe");
        assert_eq!(device.id, "atk:373b:11fe");
    }

    #[test]
    fn descriptor_generation_lists_only_verified_capabilities() {
        let (_, device) = enumerated(true);
        let ids: Vec<&str> = device
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert_eq!(
            ids,
            [
                "battery",
                "charging",
                "wireless_link",
                "firmware",
                "report_rate",
                "dpi_slot",
                "conflict"
            ]
        );

        let battery = device.capability("battery").expect("battery exists");
        assert_eq!(battery.cap_type, "mouse.battery");
        assert_eq!(
            battery.kind,
            CapabilityKind::Int {
                min: 0,
                max: 100,
                step: 1,
                unit: Some("%".to_string()),
            }
        );
        assert_eq!(battery.access, Access::ReadOnly);
        assert_eq!(battery.cost.latency_ms, 120);

        let firmware = device.capability("firmware").expect("firmware exists");
        assert_eq!(firmware.kind, CapabilityKind::Text { max_len: 32 });

        let report_rate = device.capability("report_rate").expect("rate exists");
        match &report_rate.kind {
            CapabilityKind::Enum { options } => {
                assert_eq!(options.len(), 7);
                assert!(options.iter().any(|option| option.label == "250 Hz"));
            }
            other => panic!("report_rate should be an enum, got {other:?}"),
        }

        let conflict = device.capability("conflict").expect("conflict exists");
        assert_eq!(conflict.label, "ATK HUB is running");
        assert_eq!(conflict.cost.latency_ms, 0);

        assert!(
            device.capability("dpi").is_none(),
            "real DPI is not decoded yet, so it must not be advertised"
        );
        assert!(device.capability_by_type("mouse.dpi").is_none());
    }

    #[test]
    fn conflict_capability_tracks_the_process_observation() {
        let (_, quiet) = enumerated(false);
        assert!(quiet.capability("conflict").is_none());
        assert!(quiet.capability_by_type("mouse.conflict").is_none());

        let (_, busy) = enumerated(true);
        assert!(busy.capability("conflict").is_some());
    }

    #[test]
    fn read_battery_returns_the_decoded_percentage() {
        let (mut core, device) = enumerated(false);
        core.transport.replies.push_back(Ok(BATTERY_REPLY.to_vec()));
        let value = core
            .read(&device.id, "battery")
            .expect("fake battery read succeeds");
        assert_eq!(value, CapabilityValue::Int(100));
        // The request written before the read is a battery query.
        assert_eq!(
            core.transport.writes.last().expect("one write"),
            &protocol::query_battery()
        );
    }

    #[test]
    fn stale_reply_addressed_to_another_command_is_skipped() {
        let (mut core, device) = enumerated(false);
        // ATK HUB polls the same collection, and HID delivers its version reply
        // to our handle too. The battery read must skip that frame, not fail.
        core.transport.replies.push_back(Ok(VERSION_REPLY.to_vec()));
        core.transport.replies.push_back(Ok(BATTERY_REPLY.to_vec()));
        let value = core
            .read(&device.id, "battery")
            .expect("battery read skips the foreign reply");
        assert_eq!(value, CapabilityValue::Int(100));
    }

    #[test]
    fn read_every_capability() {
        let (mut core, device) = enumerated(true);
        core.transport.replies.push_back(Ok(BATTERY_REPLY.to_vec()));
        assert_eq!(
            core.read(&device.id, "charging").expect("charging reads"),
            CapabilityValue::Bool(false)
        );
        core.transport
            .replies
            .push_back(Ok(WIRELESS_REPLY.to_vec()));
        assert_eq!(
            core.read(&device.id, "wireless_link").expect("link reads"),
            CapabilityValue::Bool(true)
        );
        core.transport.replies.push_back(Ok(VERSION_REPLY.to_vec()));
        assert_eq!(
            core.read(&device.id, "firmware").expect("firmware reads"),
            CapabilityValue::Text("3.3".to_string())
        );
        core.transport.replies.push_back(Ok(EEPROM_REPLY.to_vec()));
        assert_eq!(
            core.read(&device.id, "report_rate").expect("rate reads"),
            CapabilityValue::Enum("250".to_string())
        );
        core.transport.replies.push_back(Ok(EEPROM_REPLY.to_vec()));
        assert_eq!(
            core.read(&device.id, "dpi_slot").expect("slot reads"),
            CapabilityValue::Int(1)
        );
        assert_eq!(
            core.read(&device.id, "conflict").expect("conflict reads"),
            CapabilityValue::Bool(true)
        );
    }

    #[test]
    fn write_is_rejected_as_read_only() {
        let (mut core, device) = enumerated(false);
        let error = core
            .write(&device.id, "battery", &CapabilityValue::Int(50))
            .expect_err("writes are not implemented");
        assert_eq!(error.kind, ErrorKind::ReadOnly);
        assert!(error.to_string().contains("read-only"));
        assert!(
            core.transport.writes.is_empty() || core.transport.writes.len() == 4,
            "a rejected write must not reach the device"
        );
    }

    #[test]
    fn read_of_an_unadvertised_capability_is_unknown() {
        let (mut core, device) = enumerated(false);
        let error = core
            .read(&device.id, "dpi")
            .expect_err("dpi is not advertised");
        assert_eq!(error.kind, ErrorKind::UnknownCapability);
    }

    #[test]
    fn silent_collection_is_not_exposed() {
        let keyboard = RawDevice {
            path: "\\\\?\\hid#vid_373b&pid_1258&mi_01#test".to_string(),
            serial: Some("0000001a0795f68e".to_string()),
            product: Some("RS7 Air".to_string()),
            vendor_id: 0x373B,
            product_id: 0x1258,
            usage_page: 0xFF60,
            usage: 0x0061,
            input_report_len: 385,
            output_report_len: 385,
        };
        let transport = FakeTransport::new(
            vec![keyboard],
            vec![Err(HubError::new(ErrorKind::Timeout, "no reply"))],
        );
        let mut core = Core::new(transport);
        let devices = core.enumerate_with(false).expect("enumeration succeeds");
        assert!(devices.is_empty());
    }

    #[test]
    fn silent_mouse_with_hub_running_exposes_conflict_only() {
        // The probe makes three quick battery attempts before giving up.
        let transport = FakeTransport::new(
            vec![mouse()],
            vec![
                Err(HubError::new(ErrorKind::Timeout, "asleep")),
                Err(HubError::new(ErrorKind::Timeout, "asleep")),
                Err(HubError::new(ErrorKind::Timeout, "asleep")),
            ],
        );
        let mut core = Core::new(transport);
        let devices = core.enumerate_with(true).expect("enumeration succeeds");
        assert_eq!(devices.len(), 1);
        let ids: Vec<&str> = devices[0]
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert_eq!(ids, ["conflict"]);
        assert_eq!(
            core.read(&devices[0].id, "conflict")
                .expect("conflict reads"),
            CapabilityValue::Bool(true)
        );
    }

    #[test]
    fn garbage_first_reply_is_not_exposed() {
        let transport = FakeTransport::new(
            vec![mouse()],
            vec![Ok(vec![0x00, 0x04, 0x00, 0x00, 0x00, 0x00])],
        );
        let mut core = Core::new(transport);
        let devices = core.enumerate_with(false).expect("enumeration succeeds");
        assert!(devices.is_empty());
    }

    #[test]
    fn adapter_info_names_the_binary_contract() {
        let adapter = AtkAdapter::new();
        let info = adapter.info();
        assert_eq!(info.id, "atk");
        assert_eq!(info.name, "ATK (HID)");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.kind, AdapterKind::OutOfProcess);
    }
}
