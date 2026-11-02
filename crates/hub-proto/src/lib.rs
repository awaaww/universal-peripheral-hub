//! Wire protocol between `hubd` and the UI shell.
//!
//! Transport: one named pipe per user, UTF-8 JSON-RPC 2.0 objects separated by
//! `\n`. `error.code` is always an integer (JSON-RPC 2.0 requires it); the
//! machine-readable error class travels in `error.data.kind`.

use std::collections::BTreeMap;

use hub_core::{
    AdapterState, ApplyReport, CapabilityValue, DeviceDescriptor, HubError, ScenesFile,
    WriteOutcome,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

pub const PROTOCOL_VERSION: u32 = 1;

/// Hard limit for one request line. A client that exceeds it is disconnected
/// rather than allowed to grow the server's memory without bound.
pub const MAX_LINE_BYTES: usize = 1024 * 1024;

/// Per-user pipe name. Includes the user name so two Windows sessions do not
/// share a bus; the single-instance mutex remains per session.
pub fn pipe_name() -> String {
    pipe_name_for(&user_slug())
}

pub fn pipe_name_for(user: &str) -> String {
    format!(r"\\.\pipe\universal-peripheral-hub-{user}")
}

fn user_slug() -> String {
    let raw = std::env::var("USERNAME").unwrap_or_else(|_| "default".into());
    let slug: String = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    if slug.is_empty() {
        "default".into()
    } else {
        slug
    }
}

pub mod method {
    pub const HELLO: &str = "hub.hello";
    pub const DEVICES_LIST: &str = "devices.list";
    pub const CAPABILITY_READ: &str = "capability.read";
    pub const CAPABILITY_WRITE: &str = "capability.write";
    pub const ADAPTER_SET_ENABLED: &str = "adapter.set_enabled";
    /// Stop (or resume) talking to one device. The persisted token is
    /// adapter-specific and opaque to the client.
    pub const DEVICE_SET_BLOCKED: &str = "device.set_blocked";
    /// Read the scene document together with what the engine is doing.
    pub const SCENES_GET: &str = "scenes.get";
    /// Replace the whole document. `mode` decides whether the change is
    /// recomputed against the current foreground executable right away.
    pub const SCENES_SET: &str = "scenes.set";
    /// Apply one scene because the user asked for it.
    pub const SCENE_APPLY: &str = "scene.apply";
    /// Notification sent by a second `hubd` instance (or by a shortcut) asking
    /// the running instance to show the panel.
    pub const ACTIVATE: &str = "hub.activate";
    /// Server push: the visible device set changed.
    pub const DEVICES_CHANGED: &str = "devices.changed";
    /// Server push: the scene document or the runtime state changed.
    pub const SCENES_CHANGED: &str = "scenes.changed";
}

pub mod codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// Domain error; `data.kind` carries the hub error class.
    pub const HUB_ERROR: i64 = -32000;
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    #[serde(default = "jsonrpc_version")]
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

fn jsonrpc_version() -> String {
    "2.0".to_string()
}

impl Request {
    pub fn call(id: u64, method: &str, params: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: Some(id),
            method: method.to_string(),
            params,
        }
    }

    pub fn notification(method: &str, params: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id: None,
            method: method.to_string(),
            params,
        }
    }

    pub fn is_notification(&self) -> bool {
        self.id.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn success(id: Option<u64>, result: Value) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: Option<u64>, error: RpcError) -> Self {
        Self {
            jsonrpc: jsonrpc_version(),
            id,
            result: None,
            error: Some(error),
        }
    }

    pub fn parse_error(message: impl Into<String>) -> Self {
        Self::failure(
            None,
            RpcError::new(codes::PARSE_ERROR, message.into(), None),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<ErrorData>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorData {
    /// `hub_core::ErrorKind` as a snake_case string, e.g. `unknown_device`.
    pub kind: String,
    pub retryable: bool,
}

impl RpcError {
    pub fn new(code: i64, message: String, data: Option<ErrorData>) -> Self {
        Self {
            code,
            message,
            data,
        }
    }

    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            codes::METHOD_NOT_FOUND,
            format!("method `{method}` is not implemented by this hub"),
            None,
        )
    }

    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(codes::INVALID_PARAMS, message.into(), None)
    }
}

impl From<HubError> for RpcError {
    fn from(err: HubError) -> Self {
        Self::new(
            codes::HUB_ERROR,
            err.message.clone(),
            Some(ErrorData {
                kind: err.kind.as_str().to_string(),
                retryable: err.kind.retryable(),
            }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HelloResult {
    pub hub: String,
    pub version: String,
    pub protocol: u32,
    pub pid: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DevicesResult {
    pub devices: Vec<DeviceDescriptor>,
    pub adapters: Vec<AdapterState>,
    pub revision: u64,
    /// Devices the user has told us not to talk to, per adapter id. The
    /// tokens are opaque: the DDC adapter uses EDID keys such as `SKG2725`,
    /// and the panel only ever echoes them back.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub blocked: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadParams {
    pub device: String,
    pub capability: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WriteParams {
    pub device: String,
    pub capability: String,
    pub value: CapabilityValue,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetEnabledParams {
    pub adapter: String,
    pub enabled: bool,
}

/// Address a device either by its id (when it is visible) or by the persisted
/// `(adapter, token)` pair (when it is blocked and therefore not enumerated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetBlockedParams {
    #[serde(default)]
    pub device: Option<String>,
    #[serde(default)]
    pub adapter: Option<String>,
    #[serde(default)]
    pub token: Option<String>,
    pub blocked: bool,
}

impl SetBlockedParams {
    /// The adapter id and its opaque block token, whichever way the client
    /// addressed the device.
    pub fn target(&self) -> Result<(String, String), HubError> {
        match (&self.device, &self.adapter, &self.token) {
            (Some(device), _, _) => {
                let (adapter, token) = device.split_once(':').ok_or_else(|| {
                    HubError::invalid_value(format!("`{device}` is not a device id"))
                })?;
                Ok((adapter.to_string(), token.to_string()))
            }
            (None, Some(adapter), Some(token)) => Ok((adapter.clone(), token.clone())),
            _ => Err(HubError::invalid_value(
                "send either `device` or both `adapter` and `token`",
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReadResult {
    pub value: CapabilityValue,
}

pub type CapabilityWriteResult = WriteOutcome;

/// What the panel needs to render the scenes section: the document it may
/// edit, plus a read-only view of what the engine last did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScenesResult {
    pub document: ScenesFile,
    pub runtime: SceneRuntime,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct SceneRuntime {
    /// False until the first foreground event established a baseline. Nothing
    /// is written before that, by design.
    pub armed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_scene: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_exe: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_applied_ms: Option<u64>,
    /// The last apply, including what it skipped or failed to write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_report: Option<ApplyReport>,
    /// `startup` (baseline only), `rule` or `manual`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_source: Option<String>,
}

/// `map` re-evaluates the current foreground executable (use it when rules
/// changed); `save` only persists the document (use it while editing values).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SceneMode {
    Map,
    Save,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SetScenesParams {
    pub document: ScenesFile,
    pub mode: SceneMode,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ApplySceneParams {
    pub scene: String,
}

/// Notification payload for `scenes.changed`.
pub type ScenesChanged = ScenesResult;

pub fn scenes_changed_notification(result: &ScenesResult) -> Request {
    Request::notification(
        method::SCENES_CHANGED,
        serde_json::to_value(result).unwrap_or(Value::Null),
    )
}

/// Notification payload for `devices.changed`.
pub type DevicesChanged = DevicesResult;

pub fn devices_changed_notification(result: &DevicesResult) -> Request {
    Request::notification(
        method::DEVICES_CHANGED,
        serde_json::to_value(result).unwrap_or(Value::Null),
    )
}

pub fn hello(params: HelloResult) -> Response {
    Response::success(None, json!(params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_round_trips_over_one_line() {
        let request = Request::call(
            7,
            method::CAPABILITY_READ,
            json!({"device": "a", "capability": "b"}),
        );
        let line = serde_json::to_string(&request).unwrap();
        assert!(!line.contains('\n'));
        let parsed: Request = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed, request);
        assert!(!parsed.is_notification());
    }

    #[test]
    fn notifications_have_no_id() {
        let request = Request::notification(method::ACTIVATE, Value::Null);
        assert!(request.is_notification());
        let line = serde_json::to_string(&request).unwrap();
        assert!(!line.contains("\"id\""));
    }

    #[test]
    fn hub_errors_keep_an_integer_code_and_a_kind() {
        let err: RpcError = HubError::unknown_device("nope").into();
        assert_eq!(err.code, codes::HUB_ERROR);
        let data = err.data.unwrap();
        assert_eq!(data.kind, "unknown_device");
        assert!(!data.retryable);
    }

    #[test]
    fn pipe_name_is_per_user_and_sanitised() {
        assert_eq!(
            pipe_name_for("alice"),
            r"\\.\pipe\universal-peripheral-hub-alice"
        );
    }
}
