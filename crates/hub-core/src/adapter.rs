use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::device::DeviceDescriptor;
use crate::error::Result;
use crate::value::CapabilityValue;

/// Where an adapter runs. First-party adapters are compiled into `hubd`; a
/// third-party or experimental adapter will run in a child process speaking
/// stdio JSON-RPC (not implemented yet, tracked for M3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    InProcess,
    OutOfProcess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub kind: AdapterKind,
}

/// A source of devices.
///
/// Calls are serialised by the hub, so implementations do not need internal
/// synchronisation. Reads and writes may block (DDC/CI calls take tens to
/// hundreds of milliseconds); the hub runs them on a blocking worker.
pub trait Adapter: Send {
    fn info(&self) -> AdapterInfo;

    /// Enumerate the currently attached devices. Called on load and whenever
    /// the hub decides to refresh; implementations must not assume a previous
    /// call happened.
    fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>>;

    fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue>;

    fn write(&mut self, device: &str, capability: &str, value: &CapabilityValue) -> Result<()>;
}

/// Constructor for an adapter instance. Kept as a closure so registering an
/// adapter does not require an instance and unloading fully drops its state.
pub type AdapterFactory = Arc<dyn Fn() -> Box<dyn Adapter> + Send + Sync>;
