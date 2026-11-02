//! DDC/CI monitor adapter.
//!
//! Layering:
//!
//! * [`dxva2`] — hand-written FFI for `dxva2.dll` (not exposed by windows-sys).
//! * [`win`] — the Win32 calls we actually use, returning raw error codes.
//! * [`edid`] — turns the registry EDID blob into a stable monitor key.
//! * [`caps`] — parses the MCCS capabilities string.
//! * [`transport`] — the seam between policy and hardware.
//! * [`adapter`] — policy: probing, retries, negative cache, descriptors.

pub mod adapter;
pub mod caps;
pub mod config;
pub mod dxva2;
pub mod edid;
pub mod transport;
pub mod win;

pub use adapter::{adapter_info, factory, DdcAdapter};
