//! The boundary between DDC policy and the Windows calls.
//!
//! `DdcAdapter` is generic over [`Transport`] so the probe/retry/caching policy
//! can be unit-tested against a fake monitor without any hardware.

use crate::dxva2::PhysicalMonitor;
use crate::edid::EdidIdentity;
use crate::win::{self, DdcResult};

/// One physical monitor found during enumeration.
#[derive(Debug, Clone)]
pub struct RawMonitor {
    /// Stable key derived from EDID (`SKG2725`); a device-path hash when EDID
    /// is unreadable.
    pub key: String,
    /// `true` when `key` is not derived from EDID and may change.
    pub weak: bool,
    pub identity: Option<EdidIdentity>,
    pub description: String,
    pub handle: isize,
}

pub trait Transport: Send {
    /// Enumerates physical monitors. Handles returned stay valid until the next
    /// call or until the transport is dropped.
    fn monitors(&mut self) -> DdcResult<Vec<RawMonitor>>;
    fn read_vcp(&mut self, handle: isize, code: u8) -> DdcResult<(u32, u32)>;
    fn write_vcp(&mut self, handle: isize, code: u8, value: u32) -> DdcResult<()>;
    fn capabilities(&mut self, handle: isize) -> DdcResult<String>;
}

/// Real implementation: `EnumDisplayMonitors` + `dxva2.dll`.
pub struct WinTransport {
    /// Owns the `PHYSICAL_MONITOR` arrays handed out by dxva2; destroying them
    /// invalidates every handle from the previous enumeration.
    held: Vec<PhysicalMonitor>,
}

impl WinTransport {
    pub fn new() -> Self {
        Self { held: Vec::new() }
    }

    fn release(&mut self) {
        win::destroy_physical_monitors(&mut self.held);
        self.held.clear();
    }
}

impl Default for WinTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WinTransport {
    fn drop(&mut self) {
        self.release();
    }
}

impl Transport for WinTransport {
    fn monitors(&mut self) -> DdcResult<Vec<RawMonitor>> {
        self.release();

        let mut out = Vec::new();
        for output in win::display_outputs() {
            let mut physical = match win::physical_monitors(output.hmonitor) {
                Ok(physical) => physical,
                Err(error) => {
                    // One broken output must not hide the others.
                    tracing::warn!(
                        output = %output.display_name,
                        %error,
                        "cannot enumerate physical monitors"
                    );
                    continue;
                }
            };

            for monitor in &physical {
                let identity = output.identity.clone();
                let key = match &identity {
                    Some(identity) => identity.key.clone(),
                    // Without EDID the device id is all we have. Mark it weak so
                    // stored per-model settings are not silently reused.
                    None => fallback_key(&output.device_id),
                };
                out.push(RawMonitor {
                    weak: identity.is_none(),
                    key,
                    identity,
                    description: monitor.description(),
                    handle: monitor.handle,
                });
            }

            self.held.append(&mut physical);
        }

        Ok(out)
    }

    fn read_vcp(&mut self, handle: isize, code: u8) -> DdcResult<(u32, u32)> {
        win::read_vcp(handle, code)
    }

    fn write_vcp(&mut self, handle: isize, code: u8, value: u32) -> DdcResult<()> {
        win::write_vcp(handle, code, value)
    }

    fn capabilities(&mut self, handle: isize) -> DdcResult<String> {
        win::read_capabilities(handle)
    }
}

/// Deterministic short key for a monitor whose EDID could not be read.
fn fallback_key(device_id: &str) -> String {
    // FNV-1a keeps the key stable across runs without pulling in a hasher crate.
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in device_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("PATH{hash:016X}")
}

#[cfg(test)]
mod tests {
    use super::fallback_key;

    #[test]
    fn fallback_key_is_stable_and_short() {
        let a = fallback_key(r"MONITOR\UNKNOWN\0001");
        let b = fallback_key(r"MONITOR\UNKNOWN\0001");
        let c = fallback_key(r"MONITOR\UNKNOWN\0002");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!(a.starts_with("PATH"));
        assert_eq!(a.len(), 20);
    }
}
