use serde::{Deserialize, Serialize};

use crate::capability::CapabilityDescriptor;
use crate::error::{HubError, Result};

/// One physical or virtual device as reported by an adapter.
///
/// `id` must be globally unique while the adapter is loaded (for example
/// `mock:display-1`); the hub rejects duplicates instead of silently shadowing
/// one device with another.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDescriptor {
    pub id: String,
    pub adapter: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vendor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub serial: Option<String>,
    pub capabilities: Vec<CapabilityDescriptor>,
}

impl DeviceDescriptor {
    pub fn capability(&self, id: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities.iter().find(|cap| cap.id == id)
    }

    pub fn capability_by_type(&self, cap_type: &str) -> Option<&CapabilityDescriptor> {
        self.capabilities
            .iter()
            .find(|cap| cap.cap_type == cap_type)
    }

    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(HubError::invalid_value("device id must not be empty"));
        }
        if self.adapter.trim().is_empty() {
            return Err(HubError::invalid_value(format!(
                "device `{}`: adapter must not be empty",
                self.id
            )));
        }
        if self.name.trim().is_empty() {
            return Err(HubError::invalid_value(format!(
                "device `{}`: name must not be empty",
                self.id
            )));
        }
        if self.capabilities.is_empty() {
            return Err(HubError::invalid_value(format!(
                "device `{}`: at least one capability is required",
                self.id
            )));
        }

        let mut ids = std::collections::BTreeSet::new();
        let mut types = std::collections::BTreeSet::new();
        for cap in &self.capabilities {
            cap.validate()?;
            if !ids.insert(cap.id.as_str()) {
                return Err(HubError::invalid_value(format!(
                    "device `{}`: duplicate capability id `{}`",
                    self.id, cap.id
                )));
            }
            if !types.insert(cap.cap_type.as_str()) {
                return Err(HubError::invalid_value(format!(
                    "device `{}`: duplicate cap_type `{}`; scenes address capabilities by type",
                    self.id, cap.cap_type
                )));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::{Access, CapabilityKind, Cost, Traits};

    fn cap(id: &str, cap_type: &str) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: id.into(),
            cap_type: cap_type.into(),
            label: id.into(),
            kind: CapabilityKind::Bool,
            access: Access::ReadWrite,
            cost: Cost::default(),
            traits: Traits::default(),
        }
    }

    fn device() -> DeviceDescriptor {
        DeviceDescriptor {
            id: "mock:one".into(),
            adapter: "mock".into(),
            name: "Mock device".into(),
            vendor: None,
            model: None,
            serial: None,
            capabilities: vec![cap("a", "display.a")],
        }
    }

    #[test]
    fn valid_device_passes() {
        assert!(device().validate().is_ok());
    }

    #[test]
    fn duplicate_cap_types_are_rejected() {
        let mut dev = device();
        dev.capabilities.push(cap("b", "display.a"));
        let err = dev.validate().unwrap_err();
        assert!(err.message.contains("duplicate cap_type"));
    }

    #[test]
    fn empty_capability_list_is_rejected() {
        let mut dev = device();
        dev.capabilities.clear();
        assert!(dev.validate().is_err());
    }
}
