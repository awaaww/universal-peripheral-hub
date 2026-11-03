//! Record and replay VIA/QMK HID traffic.
//!
//! `RecordingHid` wraps the real transport and appends every frame to a trace;
//! `ReplayHid` replaces it in tests and answers from that trace. Both keep the
//! adapter's `Transport` seam unchanged, so no protocol code knows whether it is
//! talking to a mouse or to a file.

use hub_core::{ErrorKind, HubError, Result};
use hub_trace::{ReplayLog, Trace, TraceDevice, TraceError, TraceWriter};

use crate::hid::{RawDevice, Transport, WindowsHid};

/// Turn a discovered collection into the trace's device record.
fn to_trace(device: &RawDevice) -> TraceDevice {
    TraceDevice {
        id: device.key(),
        vid: Some(device.vendor_id),
        pid: Some(device.product_id),
        usage_page: Some(device.usage_page),
        usage: Some(device.usage),
        report_len: Some(device.output_report_len),
        label: device.product.clone(),
    }
}

/// Rebuild the collection the trace recorded. The path is unknown on replay and
/// is never used: `ReplayHid::open` accepts anything.
fn from_trace(device: &TraceDevice) -> RawDevice {
    RawDevice {
        path: String::new(),
        serial: Some(device.id.clone()),
        product: device.label.clone(),
        vendor_id: device.vid.unwrap_or(0),
        product_id: device.pid.unwrap_or(0),
        usage_page: device.usage_page.unwrap_or(0),
        usage: device.usage.unwrap_or(0),
        input_report_len: device.report_len.unwrap_or(0),
        output_report_len: device.report_len.unwrap_or(0),
    }
}

fn trace_error(err: TraceError) -> HubError {
    let kind = match err {
        TraceError::SendMismatch { .. } => ErrorKind::Protocol,
        _ => ErrorKind::Internal,
    };
    HubError::new(kind, err.to_string())
}

/// Wraps the real transport, recording devices and frames as they happen.
pub struct RecordingHid {
    inner: WindowsHid,
    writer: TraceWriter,
}

impl RecordingHid {
    pub fn new(writer: TraceWriter) -> Self {
        RecordingHid {
            inner: WindowsHid::default(),
            writer,
        }
    }

    pub fn finish(self) -> Result<Trace> {
        self.writer.save().map_err(trace_error)?;
        Ok(self.writer.trace().clone())
    }
}

impl Transport for RecordingHid {
    fn devices(&mut self) -> Result<Vec<RawDevice>> {
        let devices = self.inner.devices()?;
        self.writer.devices(devices.iter().map(to_trace).collect());
        Ok(devices)
    }

    fn open(&mut self, device: &RawDevice) -> Result<()> {
        self.writer.open(&device.key());
        self.inner.open(device)
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.writer.send(frame);
        self.writer.save().map_err(trace_error)?;
        self.inner.write_frame(frame)
    }

    fn read_frame(&mut self, timeout_ms: u32) -> Result<Vec<u8>> {
        let reply = self.inner.read_frame(timeout_ms)?;
        self.writer.recv(&reply);
        self.writer.save().map_err(trace_error)?;
        Ok(reply)
    }
}

/// Answers from a recorded trace instead of a device.
pub struct ReplayHid {
    devices: Vec<RawDevice>,
    log: ReplayLog,
}

impl ReplayHid {
    pub fn new(trace: Trace) -> Self {
        let devices = trace.devices.iter().map(from_trace).collect();
        ReplayHid {
            devices,
            log: ReplayLog::new(trace),
        }
    }

    pub fn from_path(path: &std::path::Path) -> Result<Self> {
        let trace = Trace::load(path).map_err(trace_error)?;
        Ok(ReplayHid::new(trace))
    }

    /// How many recorded steps the replay has not consumed. Tests use this to
    /// assert the adapter exercised the whole recording instead of a prefix.
    pub fn remaining(&self) -> usize {
        self.log.remaining()
    }
}

impl Transport for ReplayHid {
    fn devices(&mut self) -> Result<Vec<RawDevice>> {
        Ok(self.devices.clone())
    }

    fn open(&mut self, _device: &RawDevice) -> Result<()> {
        Ok(())
    }

    fn write_frame(&mut self, frame: &[u8]) -> Result<()> {
        self.log.expect_send(frame).map_err(trace_error)
    }

    fn read_frame(&mut self, _timeout_ms: u32) -> Result<Vec<u8>> {
        self.log.next_recv().map_err(trace_error)
    }
}

#[cfg(test)]
mod tests {
    use hub_core::CapabilityValue;
    use hub_trace::Trace;

    use crate::adapter::Core;

    use super::ReplayHid;

    fn recording() -> Trace {
        serde_json::from_str(include_str!("../tests/traces/via-rs7-air.json")).unwrap()
    }

    /// The checked-in recording of the real RS7 Air. Replaying it must produce
    /// the same values as the keyboard did and consume every recorded step.
    #[test]
    fn replaying_the_rs7_recording_reproduces_its_values() {
        let mut core = Core::new(ReplayHid::new(recording()));

        let devices = core.enumerate().unwrap();
        assert_eq!(devices.len(), 1, "the recording has one keyboard");
        let device = &devices[0];

        let mut values = std::collections::BTreeMap::new();
        for capability in &device.capabilities {
            if capability.access.can_read() {
                let value = core
                    .read(&device.id, &capability.id)
                    .unwrap_or_else(|error| panic!("{}: {error}", capability.id));
                values.insert(capability.id.clone(), value);
            }
        }
        assert_eq!(
            core.transport().remaining(),
            0,
            "replay should consume the whole recording"
        );

        assert!(matches!(
            values.get("keyboard.protocol"),
            Some(CapabilityValue::Int(1))
        ));
        assert!(matches!(
            values.get("keyboard.layers"),
            Some(CapabilityValue::Int(4))
        ));
        assert!(matches!(
            values.get("keyboard.keys"),
            Some(CapabilityValue::Int(96))
        ));
        assert!(matches!(
            values.get("keyboard.macros"),
            Some(CapabilityValue::Int(8))
        ));
        assert!(matches!(
            values.get("keyboard.keymap"),
            Some(CapabilityValue::Bytes(bytes)) if bytes.len() == 768
        ));
    }

    /// The descriptor list is part of the public contract: ids, labels, kinds,
    /// costs and traits. Replaying the recording must produce it byte for byte.
    #[test]
    fn replaying_reproduces_the_descriptor_golden() {
        let mut core = Core::new(ReplayHid::new(recording()));
        let devices = core.enumerate().unwrap();

        let golden: serde_json::Value =
            serde_json::from_str(include_str!("../tests/traces/via-rs7-air.descriptors.json"))
                .unwrap();
        assert_eq!(serde_json::to_value(&devices).unwrap(), golden);
    }
}
