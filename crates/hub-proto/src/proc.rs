//! Wire types for out-of-process adapters.
//!
//! One JSON object per line over the child's stdin/stdout, UTF-8, at most
//! [`MAX_LINE_BYTES`] bytes. This module is the single definition both the host
//! and the adapter binary compile against, so the two cannot drift.
//!
//! The frames are JSON-RPC 2.0 shaped but deliberately not a general-purpose
//! server: the host always sends the request, the child always answers (except
//! for `devices.changed`, which is the child's only push).

use hub_core::{CapabilityValue, DeviceDescriptor};
use serde::{Deserialize, Serialize};

/// Bumped only when a change is not backward compatible; both sides refuse a
/// mismatch instead of guessing.
pub const PROTOCOL_VERSION: u32 = 1;

/// A line longer than this is a protocol error and drops the connection.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// How long the child gets to answer `adapter.hello`.
pub const HANDSHAKE_TIMEOUT_MS: u64 = 3_000;

/// Enumeration opens device handles; give it more room than a single read.
pub const ENUMERATE_TIMEOUT_MS: u64 = 10_000;

/// One read or write. Devices behind a dongle occasionally answer late, but a
/// request that hangs longer than this is a bug, not a slow device.
pub const CALL_TIMEOUT_MS: u64 = 5_000;

/// After this many consecutive timeouts, or one process exit, the adapter is
/// disabled until the user enables it again. Restarting on a crash loop would
/// hide the crash.
pub const MAX_CONSECUTIVE_TIMEOUTS: u32 = 3;

pub mod method {
    /// Host → child, first frame on the connection.
    pub const HELLO: &str = "adapter.hello";
    /// Host → child.
    pub const ENUMERATE: &str = "adapter.enumerate";
    /// Host → child.
    pub const READ: &str = "capability.read";
    /// Host → child.
    pub const WRITE: &str = "capability.write";
    /// Child → host: devices appeared, disappeared or changed shape.
    pub const DEVICES_CHANGED: &str = "devices.changed";
    /// Host → child, best effort before the pipe is closed.
    pub const SHUTDOWN: &str = "adapter.shutdown";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloParams {
    pub protocol: u32,
    pub hub_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    /// Stable adapter id (`atk`); becomes the prefix of every device id.
    pub id: String,
    pub name: String,
    pub version: String,
    pub protocol: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumerateResult {
    pub devices: Vec<DeviceDescriptor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadParams {
    pub device: String,
    /// Adapter-local capability id, not `cap_type`.
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadResult {
    pub value: CapabilityValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteParams {
    pub device: String,
    pub capability: String,
    pub value: CapabilityValue,
}

/// Result of a write. The host still reads back afterwards; `accepted` only
/// means the child sent the frame, so a device that ignores writes is caught by
/// the read-back, not by this flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteResult {
    #[serde(default)]
    pub accepted: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_round_trips() {
        let hello = HelloResult {
            id: "atk".into(),
            name: "ATK".into(),
            version: "0.1.0".into(),
            protocol: PROTOCOL_VERSION,
        };
        let text = serde_json::to_string(&hello).unwrap();
        assert!(text.contains("\"protocol\":1"));
        assert_eq!(serde_json::from_str::<HelloResult>(&text).unwrap(), hello);
    }

    #[test]
    fn enumerate_result_carries_capability_descriptors() {
        let json = r#"{"devices":[{"id":"atk:mouse","adapter":"atk","name":"Mouse",
            "capabilities":[{"id":"battery","cap_type":"mouse.battery","label":"Battery",
            "kind":{"type":"int","min":0,"max":100,"step":1,"unit":"%"}}]}]}"#;
        let parsed: EnumerateResult = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.devices.len(), 1);
        assert_eq!(parsed.devices[0].capabilities[0].cap_type, "mouse.battery");
    }
}
