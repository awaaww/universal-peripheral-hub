//! Adapter logic for the ATK RS7 Air in VIA/QMK raw-HID mode.
//!
//! The core is generic over [`Transport`] so the whole state machine is
//! exercised by [`FakeTransport`](tests::FakeTransport) without a keyboard.
//!
//! Everything this adapter exposes is read-only. The protocol's write side
//! (setting keycodes, lighting, macros) is deliberately not implemented in this
//! milestone; `write` fails with a stable message instead of pretending.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use hub_core::{
    Access, Adapter, AdapterInfo, AdapterKind, CapabilityDescriptor, CapabilityKind,
    CapabilityValue, Cost, DeviceDescriptor, ErrorKind, HubError, Result, Traits,
};

use crate::hid::{RawDevice, Transport, WindowsHid};
use crate::protocol;

/// Adapter id registered by `hubd`. Must match the host's helper spec.
pub const ADAPTER_ID: &str = "via";
/// Adapter name shown for this out-of-process helper.
pub const ADAPTER_NAME: &str = "ATK RS7 Air (VIA)";
/// The keyboard's vendor, as shown in the UI.
pub const DEVICE_VENDOR: &str = "ATK";
/// Stable model name for the one device this adapter exposes.
pub const DEVICE_NAME: &str = "ATK RS7 Air (VIA)";
/// Total time a single query may spend reading replies.
const READ_TIMEOUT_MS: u32 = 700;
/// Latency budget for capabilities that hit the wire.
const LATENCY_MS: u32 = 120;

const CAP_PROTOCOL: &str = "keyboard.protocol";
const CAP_LAYERS: &str = "keyboard.layers";
const CAP_KEYS: &str = "keyboard.keys";
const CAP_MACROS: &str = "keyboard.macros";

/// One enumerated device: the transport handle description plus the descriptor
/// and the derived key count that `keyboard.keys` reports.
struct DeviceState {
    raw: RawDevice,
    keys: u16,
    descriptor: DeviceDescriptor,
}

/// Transport-independent adapter core.
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

    /// Enumerate collections and probe each one for the VIA capability set.
    ///
    /// A collection that does not answer, or leaves a query unanswered, is not
    /// exposed; enumeration itself never fails because a device is silent.
    pub fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        self.devices.clear();
        let mut candidates = self.transport.devices()?;
        candidates.sort_by(|a, b| {
            a.key()
                .cmp(&b.key())
                .then_with(|| a.output_report_len.cmp(&b.output_report_len))
        });

        let mut exposed = Vec::new();
        let mut seen = BTreeSet::new();
        for raw in candidates {
            tracing::debug!(
                path = %raw.path,
                usage_page = raw.usage_page,
                usage = raw.usage,
                "VIA: candidate collection"
            );
            let id = format!("{ADAPTER_ID}:{}", device_key(&raw));
            if !seen.insert(id.clone()) {
                tracing::debug!(device = %id, "VIA: ignoring a duplicate collection");
                continue;
            }
            match self.probe(&raw) {
                Ok(keys) => {
                    let descriptor = describe(id.clone(), &raw);
                    if let Err(error) = descriptor.validate() {
                        tracing::warn!(device = %id, %error, "VIA: descriptor rejected");
                        continue;
                    }
                    tracing::debug!(device = %id, keys, "VIA: keyboard ready");
                    exposed.push(descriptor.clone());
                    self.devices.insert(
                        id,
                        DeviceState {
                            raw,
                            keys,
                            descriptor,
                        },
                    );
                }
                Err(error) => {
                    tracing::debug!(path = %raw.path, %error, "VIA: collection did not answer the VIA queries");
                }
            }
        }
        Ok(exposed)
    }

    /// Read one capability. `keyboard.keys` is answered from the value derived
    /// during enumeration; everything else is re-read from the device.
    pub fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        let (raw, keys, advertised) = {
            let state = self.devices.get(device).ok_or_else(|| {
                HubError::unknown_device(format!("unknown VIA device `{device}`"))
            })?;
            (
                state.raw.clone(),
                state.keys,
                state.descriptor.capability(capability).is_some(),
            )
        };
        if !advertised {
            return Err(HubError::unknown_capability(format!(
                "VIA device `{device}` has no capability `{capability}`"
            )));
        }

        if capability == CAP_KEYS {
            return Ok(CapabilityValue::Int(i64::from(keys)));
        }

        self.transport.open(&raw)?;
        match capability {
            CAP_PROTOCOL => {
                let version = self.query(
                    &protocol::get_protocol_version(),
                    protocol::decode_protocol_version,
                )?;
                if !(1..=16).contains(&version) {
                    return Err(HubError::new(
                        ErrorKind::Protocol,
                        format!("VIA protocol version {version} is outside 1..=16"),
                    ));
                }
                Ok(CapabilityValue::Int(i64::from(version)))
            }
            CAP_LAYERS => {
                let layers =
                    self.query(&protocol::get_layer_count(), protocol::decode_layer_count)?;
                if !(1..=32).contains(&layers) {
                    return Err(HubError::new(
                        ErrorKind::Protocol,
                        format!("VIA layer count {layers} is outside 1..=32"),
                    ));
                }
                Ok(CapabilityValue::Int(i64::from(layers)))
            }
            CAP_MACROS => {
                let macros =
                    self.query(&protocol::get_macro_count(), protocol::decode_macro_count)?;
                if macros > 64 {
                    return Err(HubError::new(
                        ErrorKind::Protocol,
                        format!("VIA macro count {macros} is outside 0..=64"),
                    ));
                }
                Ok(CapabilityValue::Int(i64::from(macros)))
            }
            other => Err(HubError::unknown_capability(format!(
                "VIA device `{device}` has no capability `{other}`"
            ))),
        }
    }

    /// The VIA adapter is read-only in this milestone: no frame that changes a
    /// setting is ever sent.
    pub fn write(
        &mut self,
        _device: &str,
        _capability: &str,
        _value: &CapabilityValue,
    ) -> Result<()> {
        Err(HubError::unsupported(
            "the VIA adapter is read-only in this milestone",
        ))
    }

    /// Probe one collection: version, layers, macros, then walk the keymap.
    ///
    /// The device is exposed only when every query that shapes a capability
    /// succeeded and decoded to a value inside the advertised range.
    fn probe(&mut self, device: &RawDevice) -> Result<u16> {
        self.transport.open(device)?;

        let version = self.query(
            &protocol::get_protocol_version(),
            protocol::decode_protocol_version,
        )?;
        if !(1..=16).contains(&version) {
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!("VIA protocol version {version} is outside 1..=16"),
            ));
        }

        let layers = self.query(&protocol::get_layer_count(), protocol::decode_layer_count)?;
        if !(1..=32).contains(&layers) {
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!("VIA layer count {layers} is outside 1..=32"),
            ));
        }

        let macros = self.query(&protocol::get_macro_count(), protocol::decode_macro_count)?;
        if macros > 64 {
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!("VIA macro count {macros} is outside 0..=64"),
            ));
        }

        let keys = self.derive_keys(layers)?;
        if !(1..=256).contains(&keys) {
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!("derived key count {keys} is outside 1..=256"),
            ));
        }
        Ok(keys)
    }

    /// Walk the keymap in [`protocol::PAGE_BYTES`] pages until the first empty
    /// page, then divide by the keycode width and the layer count.
    ///
    /// On this board the scan reads 24 non-empty pages and stops at the cap:
    /// `768 / 2 / 4 layers = 96` keys. The measured buffer is 768 bytes
    /// (`0x0000..=0x02FF`); the task text's "8 pages / 256 bytes" arithmetic
    /// would yield 32 keys, so the cap is [`protocol::MAX_KEYMAP_BYTES`].
    fn derive_keys(&mut self, layers: u8) -> Result<u16> {
        let mut keymap_bytes = protocol::MAX_KEYMAP_BYTES;
        let mut offset = 0u16;
        while offset < protocol::MAX_KEYMAP_BYTES {
            let page = self.query(&protocol::get_keymap_buffer(offset, page_size()), |reply| {
                protocol::decode_keymap(reply, offset, page_size())
            })?;
            if protocol::page_is_empty(&page) {
                // A whole page of 0x0000 is a legal run of unbound keys
                // (KC_NO), not proof that the buffer ended: this board's last
                // page (offsets 736..767) is exactly that. An empty page only
                // ends the buffer when more of the verified window follows.
                if offset + protocol::PAGE_BYTES < protocol::MAX_KEYMAP_BYTES {
                    keymap_bytes = offset;
                    break;
                }
            }
            offset += protocol::PAGE_BYTES;
        }
        Ok(keymap_bytes / 2 / u16::from(layers))
    }

    /// Send one request and read the matching reply.
    ///
    /// Frames that do not parse as the answer to this command are discarded and
    /// reading continues: ATK HUB may poll the same collection, and its replies
    /// arrive interleaved. A device that never answers ends as `Timeout`.
    fn query<U, F>(&mut self, frame: &[u8], decode: F) -> Result<U>
    where
        F: Fn(&[u8]) -> Result<U>,
    {
        let expected = frame[1];
        self.transport.write_frame(frame)?;
        let deadline = Instant::now() + Duration::from_millis(u64::from(READ_TIMEOUT_MS));
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(HubError::new(
                    ErrorKind::Timeout,
                    format!(
                        "VIA keyboard did not answer command 0x{expected:02x} within {READ_TIMEOUT_MS} ms"
                    ),
                ));
            }
            let timeout_ms = u32::try_from(remaining.as_millis()).unwrap_or(u32::MAX);
            let reply = self.transport.read_frame(timeout_ms)?;
            match decode(&reply) {
                Ok(value) => return Ok(value),
                Err(error) => {
                    tracing::debug!(%error, "VIA: discarding a reply that is not ours");
                }
            }
        }
    }
}

fn page_size() -> u8 {
    protocol::PAGE_BYTES as u8
}

/// Stable device key: the serial when present, otherwise a token derived from
/// the interface path's instance id.
fn device_key(device: &RawDevice) -> String {
    match device
        .serial
        .as_deref()
        .map(str::trim)
        .filter(|serial| !serial.is_empty())
    {
        Some(serial) => serial.to_ascii_lowercase(),
        None => path_token(&device.path),
    }
}

/// Derive a readable token from a HID interface path.
///
/// The instance id is the second-to-last `#`-separated segment; everything else
/// (the interface class and the interface GUID) is shared by every unit.
fn path_token(path: &str) -> String {
    let parts: Vec<&str> = path.split('#').collect();
    let raw = if parts.len() >= 2 {
        parts[parts.len() - 2]
    } else {
        path
    };
    let mut token = String::with_capacity(raw.len());
    for character in raw.chars() {
        if character.is_ascii_alphanumeric() {
            token.push(character.to_ascii_lowercase());
        } else if !token.is_empty() && !token.ends_with('-') {
            token.push('-');
        }
    }
    let token = token.trim_matches('-').to_string();
    if token.is_empty() {
        "device".to_string()
    } else {
        token
    }
}

fn capability(
    id: &str,
    label: &str,
    kind: CapabilityKind,
    latency_ms: u32,
) -> CapabilityDescriptor {
    CapabilityDescriptor {
        id: id.to_string(),
        cap_type: id.to_string(),
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

fn describe(id: String, raw: &RawDevice) -> DeviceDescriptor {
    DeviceDescriptor {
        id,
        adapter: ADAPTER_ID.to_string(),
        name: DEVICE_NAME.to_string(),
        vendor: Some(DEVICE_VENDOR.to_string()),
        model: raw.product.clone(),
        serial: raw.serial.clone(),
        capabilities: vec![
            capability(
                CAP_PROTOCOL,
                "Protocol version",
                CapabilityKind::Int {
                    min: 1,
                    max: 16,
                    step: 1,
                    unit: None,
                },
                LATENCY_MS,
            ),
            capability(
                CAP_LAYERS,
                "Keymap layers",
                CapabilityKind::Int {
                    min: 1,
                    max: 32,
                    step: 1,
                    unit: None,
                },
                LATENCY_MS,
            ),
            capability(
                CAP_KEYS,
                "Keys in the keymap",
                CapabilityKind::Int {
                    min: 1,
                    max: 256,
                    step: 1,
                    unit: None,
                },
                0,
            ),
            capability(
                CAP_MACROS,
                "Macros",
                CapabilityKind::Int {
                    min: 0,
                    max: 64,
                    step: 1,
                    unit: None,
                },
                LATENCY_MS,
            ),
        ],
    }
}

/// The real adapter: the core over the Win32 HID transport.
pub struct ViaAdapter {
    core: Core<WindowsHid>,
}

impl Default for ViaAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ViaAdapter {
    pub fn new() -> Self {
        Self {
            core: Core::new(WindowsHid::new()),
        }
    }
}

impl Adapter for ViaAdapter {
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
    use crate::protocol::{
        CMD_KEYMAP_BUFFER, CMD_LAYER_COUNT, CMD_MACRO_COUNT, CMD_PROTOCOL_VERSION,
        MAX_KEYMAP_BYTES, PAGE_BYTES, REPORT_ID, REPORT_LEN,
    };

    #[derive(Default)]
    struct FakeTransport {
        devices: Vec<RawDevice>,
        replies: VecDeque<Result<Vec<u8>>>,
        writes: Vec<Vec<u8>>,
    }

    impl FakeTransport {
        fn new(devices: Vec<RawDevice>, replies: VecDeque<Result<Vec<u8>>>) -> Self {
            Self {
                devices,
                replies,
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

    fn reply(command: u8, data: &[u8]) -> Result<Vec<u8>> {
        let mut frame = vec![0u8; REPORT_LEN];
        frame[0] = REPORT_ID;
        frame[1] = command;
        frame[2..2 + data.len()].copy_from_slice(data);
        Ok(frame)
    }

    fn keymap_reply(offset: u16, size: u8, keycodes: &[u8]) -> Result<Vec<u8>> {
        let [offset_hi, offset_lo] = offset.to_be_bytes();
        let mut data = vec![offset_hi, offset_lo, size];
        data.extend_from_slice(keycodes);
        reply(CMD_KEYMAP_BUFFER, &data)
    }

    fn board() -> RawDevice {
        RawDevice {
            path: "\\\\?\\hid#vid_373b&pid_1258&mi_01&col05#7&1a2b3c4d&0&0000#{4d1e55b2-f16f-11cf-88cb-001111000030}".to_string(),
            serial: Some("0000001A0795F68E".to_string()),
            product: Some("ATK RS7 Air".to_string()),
            vendor_id: 0x373B,
            product_id: 0x1258,
            usage_page: 0xFF60,
            usage: 0x0061,
            input_report_len: 385,
            output_report_len: 385,
        }
    }

    /// Replies for a complete probe: version 1, `layers` layers, `macros`
    /// macros, then the keymap pages.
    fn full_replies(layers: u8, macros: u8, keymap: &[u8]) -> VecDeque<Result<Vec<u8>>> {
        let mut replies = VecDeque::new();
        replies.push_back(reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]));
        replies.push_back(reply(CMD_LAYER_COUNT, &[layers]));
        replies.push_back(reply(CMD_MACRO_COUNT, &[macros]));
        let mut offset = 0u16;
        while offset < MAX_KEYMAP_BYTES {
            let start = usize::from(offset);
            let end = start + usize::from(PAGE_BYTES);
            let page = keymap.get(start..end).unwrap_or(&[]);
            replies.push_back(keymap_reply(offset, PAGE_BYTES as u8, page));
            offset += PAGE_BYTES;
        }
        replies
    }

    /// 768 bytes of nonzero keycodes, i.e. the measured board.
    fn full_keymap() -> Vec<u8> {
        let mut keymap = vec![0u8; usize::from(MAX_KEYMAP_BYTES)];
        for (index, byte) in keymap.iter_mut().enumerate() {
            *byte = (index % 251) as u8 + 1;
        }
        keymap
    }

    fn enumerated(
        replies: VecDeque<Result<Vec<u8>>>,
    ) -> (Core<FakeTransport>, Vec<DeviceDescriptor>) {
        let mut core = Core::new(FakeTransport::new(vec![board()], replies));
        let devices = core.enumerate().expect("enumerate");
        (core, devices)
    }

    fn capability<'a>(device: &'a DeviceDescriptor, id: &str) -> &'a CapabilityDescriptor {
        device
            .capability(id)
            .unwrap_or_else(|| panic!("missing capability `{id}`"))
    }

    #[test]
    fn device_id_uses_the_lowercase_serial() {
        let (_core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "via:0000001a0795f68e");
    }

    #[test]
    fn device_id_falls_back_to_a_path_token() {
        let mut device = board();
        device.serial = None;
        let mut core = Core::new(FakeTransport::new(
            vec![device],
            full_replies(4, 8, &full_keymap()),
        ));
        let devices = core.enumerate().expect("enumerate");
        assert_eq!(devices.len(), 1);
        assert_eq!(devices[0].id, "via:7-1a2b3c4d-0-0000");
    }

    #[test]
    fn enumerate_advertises_the_four_read_only_capabilities() {
        let (_core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let device = &devices[0];
        let ids: Vec<&str> = device
            .capabilities
            .iter()
            .map(|capability| capability.id.as_str())
            .collect();
        assert_eq!(ids, vec![CAP_PROTOCOL, CAP_LAYERS, CAP_KEYS, CAP_MACROS]);
        for advertised in &device.capabilities {
            assert_eq!(advertised.access, Access::ReadOnly);
            assert_eq!(advertised.cap_type, advertised.id);
        }
        assert_eq!(
            capability(device, CAP_PROTOCOL).kind,
            CapabilityKind::Int {
                min: 1,
                max: 16,
                step: 1,
                unit: None,
            }
        );
        assert_eq!(
            capability(device, CAP_LAYERS).kind,
            CapabilityKind::Int {
                min: 1,
                max: 32,
                step: 1,
                unit: None,
            }
        );
        assert_eq!(
            capability(device, CAP_KEYS).kind,
            CapabilityKind::Int {
                min: 1,
                max: 256,
                step: 1,
                unit: None,
            }
        );
        assert_eq!(
            capability(device, CAP_MACROS).kind,
            CapabilityKind::Int {
                min: 0,
                max: 64,
                step: 1,
                unit: None,
            }
        );
    }

    #[test]
    fn keys_are_derived_once_and_cost_nothing_to_read() {
        let (_core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        assert_eq!(capability(&devices[0], CAP_KEYS).cost.latency_ms, 0);
        assert_eq!(capability(&devices[0], CAP_LAYERS).cost.latency_ms, 120);
    }

    #[test]
    fn descriptor_names_the_board() {
        let (_core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let device = &devices[0];
        assert_eq!(device.adapter, "via");
        assert_eq!(device.name, "ATK RS7 Air (VIA)");
        assert_eq!(device.vendor.as_deref(), Some("ATK"));
        assert_eq!(device.model.as_deref(), Some("ATK RS7 Air"));
        assert_eq!(device.serial.as_deref(), Some("0000001A0795F68E"));
    }

    #[test]
    fn descriptor_validates() {
        let (_core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        devices[0].validate().expect("descriptor is valid");
    }

    #[test]
    fn keys_are_derived_from_the_keymap_buffer() {
        let (mut core, devices) = enumerated(full_replies(4, 8, &full_keymap()));
        assert_eq!(devices.len(), 1);
        assert_eq!(
            core.read("via:0000001a0795f68e", CAP_KEYS).expect("keys"),
            CapabilityValue::Int(96)
        );
    }

    #[test]
    fn a_shorter_keymap_yields_fewer_keys() {
        let mut core = Core::new(FakeTransport::new(
            vec![board()],
            full_replies(4, 8, &[0x04; 64]),
        ));
        let devices = core.enumerate().expect("enumerate");
        assert_eq!(devices.len(), 1);
        assert_eq!(
            core.read("via:0000001a0795f68e", CAP_KEYS).expect("keys"),
            CapabilityValue::Int(8)
        );
    }

    #[test]
    fn the_keymap_scan_stops_at_the_first_empty_page() {
        let mut core = Core::new(FakeTransport::new(
            vec![board()],
            full_replies(4, 8, &[0x04; 64]),
        ));
        core.enumerate().expect("enumerate");
        let writes = &core.transport.writes;
        // protocol, layers, macros, keymap at 0, keymap at 32, keymap at 64.
        assert_eq!(writes.len(), 6);
        assert_eq!(writes[5], protocol::get_keymap_buffer(64, 32));
    }

    #[test]
    fn a_trailing_unbound_page_does_not_truncate_the_keymap() {
        // The measured board leaves the last 16 keycodes of layer 4 unbound,
        // so the final 32-byte page is all zero although it is part of the
        // verified 768-byte buffer. Only an empty page followed by more of the
        // window ends the scan.
        let mut keymap = full_keymap();
        let tail = keymap.len() - usize::from(PAGE_BYTES);
        keymap[tail..].fill(0);
        let (mut core, devices) = enumerated(full_replies(4, 8, &keymap));
        assert_eq!(devices.len(), 1);
        assert_eq!(
            core.read("via:0000001a0795f68e", CAP_KEYS).expect("keys"),
            CapabilityValue::Int(96)
        );
    }

    #[test]
    fn read_protocol_version() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        core.transport
            .replies
            .push_back(reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]));
        let value = core
            .read("via:0000001a0795f68e", CAP_PROTOCOL)
            .expect("protocol");
        assert_eq!(value, CapabilityValue::Int(1));
        assert_eq!(
            core.transport.writes.last(),
            Some(&protocol::get_protocol_version())
        );
    }

    #[test]
    fn read_layer_count() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        core.transport
            .replies
            .push_back(reply(CMD_LAYER_COUNT, &[4]));
        let value = core
            .read("via:0000001a0795f68e", CAP_LAYERS)
            .expect("layers");
        assert_eq!(value, CapabilityValue::Int(4));
    }

    #[test]
    fn read_macro_count() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        core.transport
            .replies
            .push_back(reply(CMD_MACRO_COUNT, &[8]));
        let value = core
            .read("via:0000001a0795f68e", CAP_MACROS)
            .expect("macros");
        assert_eq!(value, CapabilityValue::Int(8));
    }

    #[test]
    fn reading_keys_does_not_touch_the_transport() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let writes_before = core.transport.writes.len();
        let value = core.read("via:0000001a0795f68e", CAP_KEYS).expect("keys");
        assert_eq!(value, CapabilityValue::Int(96));
        assert_eq!(core.transport.writes.len(), writes_before);
    }

    #[test]
    fn stale_replies_for_other_commands_are_discarded() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        // ATK HUB polled the same collection; its macro reply arrives first.
        core.transport
            .replies
            .push_back(reply(CMD_MACRO_COUNT, &[8]));
        core.transport
            .replies
            .push_back(reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]));
        let value = core
            .read("via:0000001a0795f68e", CAP_PROTOCOL)
            .expect("protocol after a foreign reply");
        assert_eq!(value, CapabilityValue::Int(1));
    }

    #[test]
    fn a_keymap_reply_for_another_offset_is_discarded() {
        let mut replies = VecDeque::new();
        replies.push_back(reply(CMD_PROTOCOL_VERSION, &[0x00, 0x01]));
        replies.push_back(reply(CMD_LAYER_COUNT, &[4]));
        replies.push_back(reply(CMD_MACRO_COUNT, &[8]));
        // A stale page for offset 32 arrives while offset 0 is being read.
        replies.push_back(keymap_reply(32, 32, &[0x04; 32]));
        let keymap = full_keymap();
        let mut offset = 0u16;
        while offset < MAX_KEYMAP_BYTES {
            let start = usize::from(offset);
            replies.push_back(keymap_reply(
                offset,
                PAGE_BYTES as u8,
                &keymap[start..start + usize::from(PAGE_BYTES)],
            ));
            offset += PAGE_BYTES;
        }
        let (mut core, devices) = enumerated(replies);
        assert_eq!(devices.len(), 1);
        assert_eq!(
            core.read("via:0000001a0795f68e", CAP_KEYS),
            Ok(CapabilityValue::Int(96))
        );
    }

    #[test]
    fn an_unadvertised_capability_is_rejected() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let error = core
            .read("via:0000001a0795f68e", "keyboard.lighting")
            .expect_err("unknown capability");
        assert_eq!(error.kind, ErrorKind::UnknownCapability);
    }

    #[test]
    fn an_unknown_device_is_rejected() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let error = core
            .read("via:not-here", CAP_PROTOCOL)
            .expect_err("unknown device");
        assert_eq!(error.kind, ErrorKind::UnknownDevice);
    }

    #[test]
    fn write_is_rejected_with_the_read_only_message() {
        let (mut core, _devices) = enumerated(full_replies(4, 8, &full_keymap()));
        let writes_before = core.transport.writes.len();
        let error = core
            .write("via:0000001a0795f68e", CAP_LAYERS, &CapabilityValue::Int(2))
            .expect_err("read-only adapter");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert_eq!(
            error.message,
            "the VIA adapter is read-only in this milestone"
        );
        assert_eq!(core.transport.writes.len(), writes_before);
    }

    #[test]
    fn a_silent_collection_is_not_exposed() {
        let mut core = Core::new(FakeTransport::new(vec![board()], VecDeque::new()));
        let devices = core.enumerate().expect("enumerate is not fatal");
        assert!(devices.is_empty());
    }

    #[test]
    fn a_collection_that_only_answers_foreign_commands_is_not_exposed() {
        let mut replies = VecDeque::new();
        for _ in 0..4 {
            replies.push_back(reply(CMD_MACRO_COUNT, &[8]));
        }
        let mut core = Core::new(FakeTransport::new(vec![board()], replies));
        let devices = core.enumerate().expect("enumerate is not fatal");
        assert!(devices.is_empty());
    }

    #[test]
    fn a_device_with_zero_layers_is_not_exposed() {
        let (_core, devices) = enumerated(full_replies(0, 8, &full_keymap()));
        assert!(devices.is_empty());
    }

    #[test]
    fn a_device_with_an_out_of_range_protocol_is_not_exposed() {
        let mut replies = full_replies(4, 8, &full_keymap());
        replies[0] = reply(CMD_PROTOCOL_VERSION, &[0x00, 0x11]);
        let (_core, devices) = enumerated(replies);
        assert!(devices.is_empty());
    }

    #[test]
    fn a_device_with_out_of_range_macros_is_not_exposed() {
        let mut replies = full_replies(4, 8, &full_keymap());
        replies[2] = reply(CMD_MACRO_COUNT, &[65]);
        let (_core, devices) = enumerated(replies);
        assert!(devices.is_empty());
    }

    #[test]
    fn a_duplicate_collection_is_exposed_once() {
        let mut core = Core::new(FakeTransport::new(
            vec![board(), board()],
            full_replies(4, 8, &full_keymap()),
        ));
        let devices = core.enumerate().expect("enumerate");
        assert_eq!(devices.len(), 1);
        // The duplicate is dropped before probing. A complete probe writes one
        // frame per query: protocol, layers, macros, then 24 keymap pages.
        assert_eq!(core.transport.writes.len(), 27);
    }

    #[test]
    fn adapter_info_matches_the_host_contract() {
        let adapter = ViaAdapter::new();
        let info = adapter.info();
        assert_eq!(info.id, "via");
        assert_eq!(info.name, "ATK RS7 Air (VIA)");
        assert_eq!(info.version, env!("CARGO_PKG_VERSION"));
        assert_eq!(info.kind, AdapterKind::OutOfProcess);
    }
}
