//! Record and replay ATK HID traffic.
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
    use hub_trace::Trace;

    use crate::adapter::Core;

    use super::ReplayHid;

    /// The checked-in recording was taken while the mouse's wireless link was
    /// asleep, so the adapter must report no device: a silent bus is not an
    /// error, and the replay must still consume every recorded step.
    #[test]
    fn replaying_a_silent_recording_reports_no_device() {
        let trace: Trace =
            serde_json::from_str(include_str!("../tests/traces/atk-mouse-8k.json")).unwrap();
        let mut core = Core::new(ReplayHid::new(trace));

        let devices = core.enumerate_with(false).unwrap();
        assert!(devices.is_empty());
        assert_eq!(
            core.transport().remaining(),
            0,
            "replay should consume the whole recording"
        );
    }
}
