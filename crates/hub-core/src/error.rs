use serde::{Deserialize, Serialize};

/// Stable, machine-readable error class. The string form is part of the wire
/// protocol, the numeric JSON-RPC code lives in `hub-proto`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The device or adapter does not support this capability at all.
    Unsupported,
    UnknownAdapter,
    UnknownDevice,
    UnknownCapability,
    /// Write attempted on a read-only capability.
    ReadOnly,
    /// Value rejected before reaching the device.
    InvalidValue,
    /// Device is busy; retrying later may succeed.
    Busy,
    Timeout,
    /// Transport-level hiccup; retryable without user action.
    Transient,
    /// The device answered, but with something we cannot interpret.
    Protocol,
    PermissionDenied,
    Io,
    /// Bug or invariant violation in the hub itself.
    Internal,
}

impl ErrorKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ErrorKind::Unsupported => "unsupported",
            ErrorKind::UnknownAdapter => "unknown_adapter",
            ErrorKind::UnknownDevice => "unknown_device",
            ErrorKind::UnknownCapability => "unknown_capability",
            ErrorKind::ReadOnly => "read_only",
            ErrorKind::InvalidValue => "invalid_value",
            ErrorKind::Busy => "busy",
            ErrorKind::Timeout => "timeout",
            ErrorKind::Transient => "transient",
            ErrorKind::Protocol => "protocol",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::Io => "io",
            ErrorKind::Internal => "internal",
        }
    }

    pub fn retryable(self) -> bool {
        matches!(
            self,
            ErrorKind::Busy | ErrorKind::Timeout | ErrorKind::Transient | ErrorKind::Io
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HubError {
    pub kind: ErrorKind,
    pub message: String,
}

impl HubError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    pub fn unknown_device(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnknownDevice, message)
    }

    pub fn unknown_capability(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::UnknownCapability, message)
    }

    pub fn invalid_value(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidValue, message)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Internal, message)
    }
}

impl std::fmt::Display for HubError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.kind.as_str(), self.message)
    }
}

impl std::error::Error for HubError {}

impl From<std::io::Error> for HubError {
    fn from(value: std::io::Error) -> Self {
        Self::new(ErrorKind::Io, value.to_string())
    }
}

impl From<serde_json::Error> for HubError {
    fn from(value: serde_json::Error) -> Self {
        Self::new(ErrorKind::Protocol, value.to_string())
    }
}

pub type Result<T> = std::result::Result<T, HubError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retryability_matches_the_documented_set() {
        assert!(ErrorKind::Busy.retryable());
        assert!(ErrorKind::Transient.retryable());
        assert!(!ErrorKind::Unsupported.retryable());
        assert!(!ErrorKind::ReadOnly.retryable());
    }

    #[test]
    fn display_includes_kind_and_message() {
        let err = HubError::invalid_value("out of range");
        assert_eq!(err.to_string(), "invalid_value: out of range");
    }
}
