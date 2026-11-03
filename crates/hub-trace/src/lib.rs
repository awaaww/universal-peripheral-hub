//! Record real adapter↔device traffic into a trace, and replay it later
//! against a fake transport.
//!
//! The point is regression without hardware: a protocol change that alters a
//! single byte should fail a test on a laptop that has no mouse plugged in.
//! Traces are JSON so they review like code, and the bytes are hex strings so a
//! diff points at the byte that moved.
//!
//! The format is deliberately dumb: an ordered list of what the adapter sent
//! and what came back. Replay is strict — a send that differs from the trace is
//! an error, not a resynchronisation — because "close enough" is how a protocol
//! drift goes unnoticed.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Bump only when a change would stop old traces from replaying correctly.
pub const TRACE_VERSION: u32 = 1;

/// One recording session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trace {
    pub version: u32,
    pub adapter: String,
    /// What the recorder knows about the machine it ran on. Kept for humans and
    /// for a future "this trace needs firmware x" check; replay does not trust it.
    #[serde(default)]
    pub host: Host,
    /// The devices the adapter saw when the recording started.
    #[serde(default)]
    pub devices: Vec<TraceDevice>,
    pub steps: Vec<Step>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Host {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceDevice {
    /// Stable id the adapter uses (`atk:…`, `via:…`).
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_page: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub report_len: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Step {
    /// The adapter opened a device before the traffic that follows.
    Open { device: String },
    /// Bytes the adapter wrote (including the report id, as sent).
    Send { bytes: String },
    /// Bytes the device returned.
    Recv { bytes: String },
    /// A human note, ignored by replay. Useful for marking what the operator did.
    Note { text: String },
}

impl Step {
    pub fn send(bytes: &[u8]) -> Self {
        Step::Send { bytes: hex(bytes) }
    }

    pub fn recv(bytes: &[u8]) -> Self {
        Step::Recv { bytes: hex(bytes) }
    }
}

/// Lower-case hex without separators: compact and unambiguous in a diff.
pub fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Parse hex back into bytes. Rejects odd lengths and non-hex characters.
pub fn unhex(text: &str) -> Result<Vec<u8>, TraceError> {
    if text.len() & 1 != 0 {
        return Err(TraceError::BadHex(text.to_owned()));
    }
    let mut out = Vec::with_capacity(text.len() / 2);
    let bytes = text.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        let pair = std::str::from_utf8(&bytes[index..index + 2])
            .map_err(|_| TraceError::BadHex(text.to_owned()))?;
        out.push(u8::from_str_radix(pair, 16).map_err(|_| TraceError::BadHex(text.to_owned()))?);
        index += 2;
    }
    Ok(out)
}

#[derive(Debug, thiserror::Error)]
pub enum TraceError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("trace is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("trace version {found} is newer than this build understands ({expected})")]
    Version { found: u32, expected: u32 },
    #[error("{0} is not valid hex")]
    BadHex(String),
    #[error("replay: expected to send {expected}, the adapter sent {actual}")]
    SendMismatch { expected: String, actual: String },
    #[error("replay: the adapter read while the trace has nothing left to deliver")]
    OutOfReads,
    #[error("replay: the trace ended before the adapter finished")]
    OutOfSteps,
}

/// Writes steps as they happen. `save` rewrites the whole file atomically, so a
/// crashed recording leaves the previous file intact rather than half a trace.
pub struct TraceWriter {
    trace: Trace,
    path: Option<PathBuf>,
}

impl TraceWriter {
    pub fn new(adapter: impl Into<String>) -> Self {
        TraceWriter {
            trace: Trace {
                version: TRACE_VERSION,
                adapter: adapter.into(),
                host: Host {
                    os: Some(std::env::consts::OS.to_owned()),
                    note: None,
                },
                devices: Vec::new(),
                steps: Vec::new(),
            },
            path: None,
        }
    }

    pub fn with_path(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn devices(&mut self, devices: Vec<TraceDevice>) {
        self.trace.devices = devices;
    }

    pub fn open(&mut self, device: &str) {
        self.trace.steps.push(Step::Open {
            device: device.to_owned(),
        });
    }

    pub fn send(&mut self, bytes: &[u8]) {
        self.trace.steps.push(Step::send(bytes));
    }

    pub fn recv(&mut self, bytes: &[u8]) {
        self.trace.steps.push(Step::recv(bytes));
    }

    pub fn note(&mut self, text: impl Into<String>) {
        self.trace.steps.push(Step::Note { text: text.into() });
    }

    pub fn trace(&self) -> &Trace {
        &self.trace
    }

    /// Flush to disk. Safe to call repeatedly as the recording grows.
    pub fn save(&self) -> Result<(), TraceError> {
        if let Some(path) = &self.path {
            self.trace.save(path)?;
        }
        Ok(())
    }
}

impl Trace {
    pub fn load(path: &Path) -> Result<Trace, TraceError> {
        let text = std::fs::read_to_string(path)?;
        let trace: Trace = serde_json::from_str(&text)?;
        if trace.version > TRACE_VERSION {
            return Err(TraceError::Version {
                found: trace.version,
                expected: TRACE_VERSION,
            });
        }
        Ok(trace)
    }

    pub fn save(&self, path: &Path) -> Result<(), TraceError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    }

    pub fn sends(&self) -> usize {
        self.steps
            .iter()
            .filter(|step| matches!(step, Step::Send { .. }))
            .count()
    }
}

/// Strict sequential replay: each `expect_send` must match the next send in the
/// trace, and each `next_recv` returns the next recorded reply.
pub struct ReplayLog {
    steps: Vec<Step>,
    cursor: usize,
}

impl ReplayLog {
    pub fn new(trace: Trace) -> Self {
        ReplayLog {
            steps: trace.steps,
            cursor: 0,
        }
    }

    pub fn from_path(path: &Path) -> Result<(Trace, ReplayLog), TraceError> {
        let trace = Trace::load(path)?;
        let log = ReplayLog::new(trace.clone());
        Ok((trace, log))
    }

    /// Compare what the adapter is about to send with what the trace recorded.
    /// Notes are skipped; a different send is a hard error.
    pub fn expect_send(&mut self, bytes: &[u8]) -> Result<(), TraceError> {
        loop {
            match self.steps.get(self.cursor) {
                Some(Step::Note { .. }) => self.cursor += 1,
                Some(Step::Send { bytes: expected }) => {
                    if unhex(expected)? != bytes {
                        return Err(TraceError::SendMismatch {
                            expected: expected.clone(),
                            actual: hex(bytes),
                        });
                    }
                    self.cursor += 1;
                    return Ok(());
                }
                Some(Step::Recv { .. }) => {
                    // Reaching a reply means the adapter skipped a send it used
                    // to make; report the mismatch rather than skipping.
                    return Err(TraceError::SendMismatch {
                        expected: "<a send>".to_owned(),
                        actual: hex(bytes),
                    });
                }
                Some(Step::Open { .. }) => self.cursor += 1,
                None => return Err(TraceError::OutOfSteps),
            }
        }
    }

    /// Hand back the next recorded reply.
    pub fn next_recv(&mut self) -> Result<Vec<u8>, TraceError> {
        loop {
            match self.steps.get(self.cursor) {
                Some(Step::Note { .. }) | Some(Step::Open { .. }) => self.cursor += 1,
                Some(Step::Recv { bytes }) => {
                    let bytes = unhex(bytes)?;
                    self.cursor += 1;
                    return Ok(bytes);
                }
                Some(Step::Send { .. }) => {
                    // A send before the expected reply means the adapter did not
                    // read where it used to; replay stops here instead of
                    // silently returning bytes for a different request.
                    return Err(TraceError::OutOfReads);
                }
                None => return Err(TraceError::OutOfReads),
            }
        }
    }

    pub fn remaining(&self) -> usize {
        self.steps.len().saturating_sub(self.cursor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Trace {
        Trace {
            version: TRACE_VERSION,
            adapter: "atk".into(),
            host: Host::default(),
            devices: vec![TraceDevice {
                id: "atk:1".into(),
                vid: Some(0x373b),
                pid: Some(0x11fe),
                usage_page: Some(0xff02),
                usage: Some(2),
                report_len: Some(17),
                label: Some("test".into()),
            }],
            steps: vec![
                Step::Open {
                    device: "atk:1".into(),
                },
                Step::send(&[0x08, 0x04]),
                Step::recv(&[0x08, 0x04, 0x00]),
                Step::Note {
                    text: "operator moved the mouse".into(),
                },
                Step::send(&[0x08, 0x03]),
                Step::recv(&[0x08, 0x03, 0x01]),
            ],
        }
    }

    #[test]
    fn hex_round_trips() {
        let bytes = [0u8, 1, 0x7f, 0x80, 0xff];
        assert_eq!(unhex(&hex(&bytes)).unwrap(), bytes);
        assert!(matches!(unhex("abc"), Err(TraceError::BadHex(_))));
        assert!(matches!(unhex("zz"), Err(TraceError::BadHex(_))));
    }

    #[test]
    fn save_and_load_round_trip() {
        let dir = std::env::temp_dir().join(format!("hub-trace-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("atk.json");
        sample().save(&path).unwrap();
        let loaded = Trace::load(&path).unwrap();
        assert_eq!(loaded.devices, sample().devices);
        assert_eq!(loaded.steps.len(), sample().steps.len());
        assert_eq!(loaded.sends(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn replay_matches_sends_and_delivers_reads() {
        let mut log = ReplayLog::new(sample());
        log.expect_send(&[0x08, 0x04]).unwrap();
        assert_eq!(log.next_recv().unwrap(), vec![0x08, 0x04, 0x00]);
        log.expect_send(&[0x08, 0x03]).unwrap();
        assert_eq!(log.next_recv().unwrap(), vec![0x08, 0x03, 0x01]);
        assert_eq!(log.remaining(), 0);
    }

    #[test]
    fn replay_rejects_a_changed_byte() {
        let mut log = ReplayLog::new(sample());
        let err = log.expect_send(&[0x08, 0x05]).unwrap_err();
        match err {
            TraceError::SendMismatch { expected, actual } => {
                assert_eq!(expected, "0804");
                assert_eq!(actual, "0805");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn replay_rejects_a_read_without_a_trace_reply() {
        let mut log = ReplayLog::new(sample());
        log.expect_send(&[0x08, 0x04]).unwrap();
        log.next_recv().unwrap();
        // The adapter reads again without sending; the trace has a send next.
        assert!(matches!(log.next_recv(), Err(TraceError::OutOfReads)));
    }

    #[test]
    fn writer_records_the_session() {
        let mut writer = TraceWriter::new("via");
        writer.devices(vec![TraceDevice {
            id: "via:1".into(),
            vid: None,
            pid: None,
            usage_page: None,
            usage: None,
            report_len: None,
            label: None,
        }]);
        writer.open("via:1");
        writer.send(&[0, 0x11]);
        writer.recv(&[0, 0x11, 4]);
        writer.note("layers");
        assert_eq!(writer.trace().sends(), 1);
        assert_eq!(writer.trace().steps.len(), 4);
    }
}
