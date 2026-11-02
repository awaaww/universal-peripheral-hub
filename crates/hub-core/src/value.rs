use serde::{Deserialize, Serialize};

use crate::capability::CapabilityKind;
use crate::error::{HubError, Result};

/// A concrete capability value. Serialised as `{"type": "int", "value": 42}`
/// so a client never has to guess the domain of a value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum CapabilityValue {
    Int(i64),
    Bool(bool),
    Enum(String),
    Rgb(Rgb),
    Bytes(Vec<u8>),
}

impl CapabilityValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            CapabilityValue::Int(_) => "int",
            CapabilityValue::Bool(_) => "bool",
            CapabilityValue::Enum(_) => "enum",
            CapabilityValue::Rgb(_) => "rgb",
            CapabilityValue::Bytes(_) => "bytes",
        }
    }

    /// Check the value against the advertised domain: the right variant, inside
    /// the int range, one of the enum options, within the byte budget.
    pub fn conforms_to(&self, id: &str, kind: &CapabilityKind) -> Result<()> {
        let mismatch = || {
            HubError::invalid_value(format!(
                "capability `{id}` expects {}, got {}",
                kind_type_name(kind),
                self.type_name()
            ))
        };
        match (self, kind) {
            (CapabilityValue::Int(value), CapabilityKind::Int { min, max, .. }) => {
                if value < min || value > max {
                    return Err(HubError::invalid_value(format!(
                        "capability `{id}`: {value} is outside [{min}, {max}]"
                    )));
                }
                Ok(())
            }
            (CapabilityValue::Bool(_), CapabilityKind::Bool) => Ok(()),
            (CapabilityValue::Enum(value), CapabilityKind::Enum { options }) => {
                if options.iter().any(|option| &option.value == value) {
                    Ok(())
                } else {
                    Err(HubError::invalid_value(format!(
                        "capability `{id}`: `{value}` is not one of the advertised options"
                    )))
                }
            }
            (CapabilityValue::Rgb(_), CapabilityKind::Rgb) => Ok(()),
            (CapabilityValue::Bytes(bytes), CapabilityKind::Bytes { max_len }) => {
                if bytes.len() as u64 > u64::from(*max_len) {
                    return Err(HubError::invalid_value(format!(
                        "capability `{id}`: {} bytes exceeds max_len {max_len}",
                        bytes.len()
                    )));
                }
                Ok(())
            }
            _ => Err(mismatch()),
        }
    }
}

fn kind_type_name(kind: &CapabilityKind) -> &'static str {
    match kind {
        CapabilityKind::Int { .. } => "int",
        CapabilityKind::Bool => "bool",
        CapabilityKind::Enum { .. } => "enum",
        CapabilityKind::Rgb => "rgb",
        CapabilityKind::Bytes { .. } => "bytes",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    pub fn new(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b }
    }

    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    pub fn from_hex(hex: &str) -> Option<Self> {
        let hex = hex.strip_prefix('#').unwrap_or(hex);
        if hex.len() != 6 {
            return None;
        }
        let parse = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).ok();
        Some(Self {
            r: parse(0..2)?,
            g: parse(2..4)?,
            b: parse(4..6)?,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capability::EnumOption;

    #[test]
    fn int_range_is_enforced() {
        let kind = CapabilityKind::Int {
            min: 0,
            max: 100,
            step: 1,
            unit: None,
        };
        assert!(CapabilityValue::Int(50).conforms_to("b", &kind).is_ok());
        assert!(CapabilityValue::Int(101).conforms_to("b", &kind).is_err());
        assert!(CapabilityValue::Int(-1).conforms_to("b", &kind).is_err());
    }

    #[test]
    fn enum_membership_is_enforced() {
        let kind = CapabilityKind::Enum {
            options: vec![EnumOption {
                value: "dp1".into(),
                label: "DisplayPort".into(),
            }],
        };
        assert!(CapabilityValue::Enum("dp1".into())
            .conforms_to("input", &kind)
            .is_ok());
        assert!(CapabilityValue::Enum("vga".into())
            .conforms_to("input", &kind)
            .is_err());
    }

    #[test]
    fn type_mismatch_is_reported() {
        let kind = CapabilityKind::Bool;
        let err = CapabilityValue::Int(1)
            .conforms_to("hdr", &kind)
            .unwrap_err();
        assert_eq!(err.kind, crate::ErrorKind::InvalidValue);
    }

    #[test]
    fn rgb_hex_round_trip() {
        let rgb = Rgb::new(18, 52, 86);
        assert_eq!(rgb.to_hex(), "#123456");
        assert_eq!(Rgb::from_hex("#123456"), Some(rgb));
        assert_eq!(Rgb::from_hex("12345"), None);
    }
}
