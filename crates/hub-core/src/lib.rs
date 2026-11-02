//! Core model of Universal Peripheral Hub.
//!
//! An adapter describes devices; a device describes capabilities; a capability
//! has a stable type (`display.brightness`), an adapter-local id, a value kind,
//! a cost model and traits. Everything the UI and the future scene engine show
//! is generated from these descriptors.

pub mod adapter;
pub mod capability;
pub mod device;
pub mod error;
pub mod hub;
pub mod scene;
pub mod settings;
pub mod value;

pub use adapter::{Adapter, AdapterFactory, AdapterInfo, AdapterKind};
pub use capability::{
    Access, CapabilityDescriptor, CapabilityKind, Cost, EnumOption, SideEffect, Traits,
};
pub use device::DeviceDescriptor;
pub use error::{ErrorKind, HubError, Result};
pub use hub::{AdapterState, Hub, WriteOutcome};
pub use scene::{
    ApplyReport, Scene, SceneItem, SceneOptions, SceneRule, ScenesFile, SCENES_VERSION,
};
pub use settings::{default_config_dir, Settings, SettingsStore};
pub use value::{CapabilityValue, Rgb};
