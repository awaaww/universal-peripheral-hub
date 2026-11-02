use serde::{Deserialize, Serialize};

use crate::error::{HubError, Result};

/// Description of one controllable (or observable) aspect of a device.
///
/// `cap_type` is the stable, cross-device identity used by scenes and bindings;
/// `id` is local to the adapter and may change freely. Two capabilities of the
/// same device must not share `cap_type`, so a scene rule can address a device
/// without knowing which firmware revision produced the descriptor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityDescriptor {
    pub id: String,
    pub cap_type: String,
    pub label: String,
    pub kind: CapabilityKind,
    #[serde(default)]
    pub access: Access,
    #[serde(default)]
    pub cost: Cost,
    #[serde(default)]
    pub traits: Traits,
}

impl CapabilityDescriptor {
    pub fn validate(&self) -> Result<()> {
        if self.id.trim().is_empty() {
            return Err(HubError::invalid_value("capability id must not be empty"));
        }
        if self.cap_type.trim().is_empty() {
            return Err(HubError::invalid_value(format!(
                "capability `{}` has an empty cap_type",
                self.id
            )));
        }
        if self.label.trim().is_empty() {
            return Err(HubError::invalid_value(format!(
                "capability `{}` has an empty label",
                self.id
            )));
        }
        match &self.kind {
            CapabilityKind::Int { min, max, step, .. } => {
                if min > max {
                    return Err(HubError::invalid_value(format!(
                        "capability `{}`: min {min} is greater than max {max}",
                        self.id
                    )));
                }
                if *step <= 0 {
                    return Err(HubError::invalid_value(format!(
                        "capability `{}`: step must be positive",
                        self.id
                    )));
                }
            }
            CapabilityKind::Enum { options } => {
                if options.is_empty() {
                    return Err(HubError::invalid_value(format!(
                        "capability `{}`: enum needs at least one option",
                        self.id
                    )));
                }
                let mut seen = std::collections::BTreeSet::new();
                for option in options {
                    if option.value.trim().is_empty() || option.label.trim().is_empty() {
                        return Err(HubError::invalid_value(format!(
                            "capability `{}`: empty enum option",
                            self.id
                        )));
                    }
                    if !seen.insert(option.value.as_str()) {
                        return Err(HubError::invalid_value(format!(
                            "capability `{}`: duplicate enum value `{}`",
                            self.id, option.value
                        )));
                    }
                }
            }
            CapabilityKind::Bytes { max_len } => {
                if *max_len == 0 {
                    return Err(HubError::invalid_value(format!(
                        "capability `{}`: bytes max_len must be positive",
                        self.id
                    )));
                }
            }
            CapabilityKind::Text { max_len } => {
                if *max_len == 0 {
                    return Err(HubError::invalid_value(format!(
                        "capability `{}`: text max_len must be positive",
                        self.id
                    )));
                }
            }
            CapabilityKind::Bool | CapabilityKind::Rgb => {}
        }
        Ok(())
    }

    /// Whether writing this capability can blank the screen, renegotiate the
    /// link or otherwise interrupt the user. Such capabilities are never
    /// applied implicitly by the scene engine.
    pub fn is_renegotiation(&self) -> bool {
        self.cost
            .side_effects
            .contains(&SideEffect::SignalRenegotiation)
    }
}

/// Value domain of a capability. The UI picks a control from this, never from
/// the capability name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CapabilityKind {
    Int {
        min: i64,
        max: i64,
        step: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unit: Option<String>,
    },
    Bool,
    Enum {
        options: Vec<EnumOption>,
    },
    Rgb,
    Bytes {
        max_len: u32,
    },
    /// Free-form, read-only text (firmware version, serial, model string). Not
    /// writable: there is no honest way to validate user input against a device
    /// that only reports it.
    Text {
        max_len: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnumOption {
    pub value: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    #[default]
    ReadWrite,
    ReadOnly,
    WriteOnly,
}

impl Access {
    pub fn can_read(self) -> bool {
        !matches!(self, Access::WriteOnly)
    }

    pub fn can_write(self) -> bool {
        !matches!(self, Access::ReadOnly)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Cost {
    /// Expected latency of one read or write, including device settle time.
    #[serde(default)]
    pub latency_ms: u32,
    #[serde(default)]
    pub side_effects: Vec<SideEffect>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// May blank the display or renegotiate the link (HDR, image mode, ...).
    SignalRenegotiation,
    /// May reroute or interrupt audio.
    AudioRouteChange,
    /// Takes exclusive access to the device for the duration of the call.
    ExclusiveAccess,
    /// Device may reset and re-enumerate.
    DeviceReset,
}

/// Capability traits. Defaults describe the common case: readable live state
/// that a scene may set, but that is not bindable to a physical input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Traits {
    pub live: bool,
    pub sceneable: bool,
    pub bindable: bool,
}

impl Default for Traits {
    fn default() -> Self {
        Self {
            live: true,
            sceneable: true,
            bindable: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor(kind: CapabilityKind) -> CapabilityDescriptor {
        CapabilityDescriptor {
            id: "brightness".into(),
            cap_type: "display.brightness".into(),
            label: "Brightness".into(),
            kind,
            access: Access::ReadWrite,
            cost: Cost::default(),
            traits: Traits::default(),
        }
    }

    #[test]
    fn rejects_inverted_int_range() {
        let cap = descriptor(CapabilityKind::Int {
            min: 100,
            max: 0,
            step: 1,
            unit: None,
        });
        assert!(cap.validate().is_err());
    }

    #[test]
    fn rejects_duplicate_enum_values() {
        let cap = descriptor(CapabilityKind::Enum {
            options: vec![
                EnumOption {
                    value: "dp1".into(),
                    label: "DisplayPort".into(),
                },
                EnumOption {
                    value: "dp1".into(),
                    label: "DisplayPort again".into(),
                },
            ],
        });
        assert!(cap.validate().is_err());
    }

    #[test]
    fn renegotiation_is_detected_from_cost() {
        let mut cap = descriptor(CapabilityKind::Bool);
        assert!(!cap.is_renegotiation());
        cap.cost.side_effects.push(SideEffect::SignalRenegotiation);
        assert!(cap.is_renegotiation());
    }

    #[test]
    fn access_flags_agree() {
        assert!(Access::ReadWrite.can_read() && Access::ReadWrite.can_write());
        assert!(Access::ReadOnly.can_read() && !Access::ReadOnly.can_write());
        assert!(!Access::WriteOnly.can_read() && Access::WriteOnly.can_write());
    }
}
