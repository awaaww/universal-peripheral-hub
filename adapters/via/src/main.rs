//! `via-adapter`: out-of-process adapter for VIA/QMK keyboards.
//!
//! One JSON object per line on stdin/stdout; logs go to stderr only, so the
//! host can treat stdout as a machine channel. See `docs/adapter-protocol.md`
//! and [`hub_proto::proc`] for the normative types.

mod adapter;
mod hid;
mod protocol;

use std::io::{BufRead, Read, Write};

use hub_core::{Adapter, CapabilityKind, CapabilityValue, HubError};
use hub_proto::codes;
use hub_proto::proc::{
    self, EnumerateResult, HelloParams, HelloResult, ReadParams, ReadResult, WriteParams,
    WriteResult,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter::ViaAdapter;

fn main() -> anyhow::Result<()> {
    init_tracing();

    match std::env::args().nth(1).as_deref() {
        None => serve(),
        Some("--probe") => probe(),
        Some("--write-selftest") => write_selftest(),
        Some(other) => {
            anyhow::bail!(
                "unexpected argument `{other}` (usage: via-adapter [--probe|--write-selftest])"
            )
        }
    }
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(filter)
        .init();
}

// ---------------------------------------------------------------------------
// stdio server
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RpcRequest {
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Debug, Serialize)]
struct RpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<ErrorData>,
}

#[derive(Debug, Serialize)]
struct ErrorData {
    kind: String,
    retryable: bool,
}

/// Why a request did not produce a result. Kept separate from [`HubError`] so
/// protocol-level mistakes (`-32602`, `-32601`) do not get the hub error class.
enum Failure {
    Hub(HubError),
    Params(String),
    MethodNotFound(String),
}

impl From<HubError> for Failure {
    fn from(error: HubError) -> Self {
        Self::Hub(error)
    }
}

fn serve() -> anyhow::Result<()> {
    let mut adapter = ViaAdapter::new();
    let stdin = std::io::stdin();
    let mut reader = stdin.lock();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut chunk = Vec::new();

    loop {
        chunk.clear();
        // `take` bounds the allocation even when the host sends a line that is
        // longer than the protocol allows; the extra byte tells truncation
        // apart from a line that is exactly at the limit.
        let read = {
            let limit = proc::MAX_LINE_BYTES as u64 + 1;
            let mut limited = reader.by_ref().take(limit);
            limited.read_until(b'\n', &mut chunk)?
        };
        if read == 0 {
            break;
        }

        let newline_terminated = chunk.last() == Some(&b'\n');
        if chunk.len() > proc::MAX_LINE_BYTES && !newline_terminated {
            emit(
                &mut out,
                &parse_error(format!("request exceeds {} bytes", proc::MAX_LINE_BYTES)),
            )?;
            break;
        }

        let Ok(text) = std::str::from_utf8(&chunk) else {
            emit(
                &mut out,
                &parse_error("request is not valid UTF-8".to_string()),
            )?;
            continue;
        };
        let line = text.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            continue;
        }

        let request: RpcRequest = match serde_json::from_str(line) {
            Ok(request) => request,
            Err(error) => {
                emit(&mut out, &parse_error(format!("invalid JSON: {error}")))?;
                continue;
            }
        };

        let id = request.id.clone();
        let shutdown = request.method == proc::method::SHUTDOWN;
        let response = respond(id.clone(), dispatch(&mut adapter, &request));
        if id.is_some() {
            emit(&mut out, &response)?;
        }
        if shutdown {
            break;
        }
    }
    Ok(())
}

fn dispatch(adapter: &mut ViaAdapter, request: &RpcRequest) -> Result<Value, Failure> {
    match request.method.as_str() {
        proc::method::HELLO => {
            let params: HelloParams = parse_params(&request.params)?;
            if params.protocol != proc::PROTOCOL_VERSION {
                return Err(Failure::Hub(HubError::unsupported(format!(
                    "protocol {} is not supported (this adapter speaks protocol {})",
                    params.protocol,
                    proc::PROTOCOL_VERSION
                ))));
            }
            to_value(HelloResult {
                id: "via".to_string(),
                name: "ATK RS7 Air (VIA)".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                protocol: proc::PROTOCOL_VERSION,
            })
        }
        proc::method::ENUMERATE => {
            let devices = adapter.enumerate()?;
            to_value(EnumerateResult { devices })
        }
        proc::method::READ => {
            let params: ReadParams = parse_params(&request.params)?;
            let value = adapter.read(&params.device, &params.capability)?;
            to_value(ReadResult { value })
        }
        proc::method::WRITE => {
            let params: WriteParams = parse_params(&request.params)?;
            adapter.write(&params.device, &params.capability, &params.value)?;
            to_value(WriteResult { accepted: true })
        }
        proc::method::SHUTDOWN => Ok(Value::Null),
        other => Err(Failure::MethodNotFound(other.to_string())),
    }
}

fn parse_params<T: serde::de::DeserializeOwned>(params: &Value) -> Result<T, Failure> {
    serde_json::from_value(params.clone()).map_err(|error| Failure::Params(error.to_string()))
}

fn to_value<T: Serialize>(value: T) -> Result<Value, Failure> {
    serde_json::to_value(value).map_err(|error| {
        Failure::Hub(HubError::internal(format!(
            "could not serialise response: {error}"
        )))
    })
}

fn respond(id: Option<Value>, outcome: Result<Value, Failure>) -> RpcResponse {
    match outcome {
        Ok(result) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        },
        Err(Failure::Hub(error)) => {
            let HubError { kind, message } = error;
            RpcResponse {
                jsonrpc: "2.0",
                id,
                result: None,
                error: Some(RpcError {
                    code: codes::HUB_ERROR,
                    message,
                    data: Some(ErrorData {
                        kind: kind.as_str().to_string(),
                        retryable: kind.retryable(),
                    }),
                }),
            }
        }
        Err(Failure::Params(message)) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: codes::INVALID_PARAMS,
                message: format!("invalid params: {message}"),
                data: None,
            }),
        },
        Err(Failure::MethodNotFound(method)) => RpcResponse {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError {
                code: codes::METHOD_NOT_FOUND,
                message: format!("unknown method `{method}`"),
                data: None,
            }),
        },
    }
}

fn parse_error(message: String) -> RpcResponse {
    RpcResponse {
        jsonrpc: "2.0",
        id: None,
        result: None,
        error: Some(RpcError {
            code: codes::PARSE_ERROR,
            message,
            data: None,
        }),
    }
}

fn emit(out: &mut impl Write, response: &RpcResponse) -> anyhow::Result<()> {
    let line = serde_json::to_string(response)?;
    writeln!(out, "{line}")?;
    out.flush()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// --probe: the demo path, no host process required
// ---------------------------------------------------------------------------

fn probe() -> anyhow::Result<()> {
    let mut adapter = ViaAdapter::new();
    let info = adapter.info();
    let devices = adapter.enumerate()?;

    println!("adapter {} - {} v{}", info.id, info.name, info.version);
    if devices.is_empty() {
        println!("no VIA/QMK keyboard answered the raw-HID protocol");
        return Ok(());
    }
    println!("{} device(s) answered", devices.len());

    for device in &devices {
        println!();
        println!("device {}", device.id);
        println!("  name    {}", device.name);
        if let Some(vendor) = &device.vendor {
            println!("  vendor  {vendor}");
        }
        if let Some(model) = &device.model {
            println!("  model   {model}");
        }
        if let Some(serial) = &device.serial {
            println!("  serial  {serial}");
        }
        println!();
        for cap in &device.capabilities {
            let access = if cap.access.can_write() {
                "read/write"
            } else {
                "read-only "
            };
            let value = adapter.read(&device.id, &cap.id);
            match value {
                Ok(value) => println!(
                    "  {:<14} {:<30} {access} {:>4} ms  => {}",
                    cap.id,
                    kind_summary(&cap.kind),
                    cap.cost.latency_ms,
                    value_summary(&value)
                ),
                Err(error) => println!(
                    "  {:<14} {:<30} {access} {:>4} ms  => error: {error}",
                    cap.id,
                    kind_summary(&cap.kind),
                    cap.cost.latency_ms
                ),
            }
        }
    }
    Ok(())
}

fn kind_summary(kind: &CapabilityKind) -> String {
    match kind {
        CapabilityKind::Int {
            min,
            max,
            step,
            unit,
        } => match unit {
            Some(unit) => format!("int {min}..{max} step {step} {unit}"),
            None => format!("int {min}..{max} step {step}"),
        },
        CapabilityKind::Bool => "bool".to_string(),
        CapabilityKind::Enum { options } => {
            let values: Vec<&str> = options.iter().map(|option| option.value.as_str()).collect();
            format!("enum [{}]", values.join(", "))
        }
        CapabilityKind::Rgb => "rgb".to_string(),
        CapabilityKind::Bytes { max_len } => format!("bytes<={max_len}"),
        CapabilityKind::Text { max_len } => format!("text<={max_len}"),
    }
}

fn value_summary(value: &CapabilityValue) -> String {
    match value {
        CapabilityValue::Int(value) => value.to_string(),
        CapabilityValue::Bool(value) => value.to_string(),
        CapabilityValue::Enum(value) => value.clone(),
        CapabilityValue::Rgb(value) => value.to_hex(),
        CapabilityValue::Bytes(value) => format!("{} bytes", value.len()),
        CapabilityValue::Text(value) => format!("\"{value}\""),
    }
}

/// `--write-selftest`: prove the keymap write path, then put the board back.
///
/// This is deliberately headless: the write transport was unverified when the
/// adapter shipped, and a self-test that restores the original keycode is safer
/// than a UI control whose semantics nobody has agreed on yet.
fn write_selftest() -> anyhow::Result<()> {
    let mut adapter = ViaAdapter::new();
    let devices = adapter.enumerate()?;
    if devices.is_empty() {
        anyhow::bail!("no VIA keyboard is connected");
    }
    let report = adapter.write_selftest()?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if report.ok {
        println!(
            "write round-trip ok: layer {}, offset 0x{:03x}, 0x{:04x} -> 0x{:04x} -> restored 0x{:04x}",
            report.layer, report.offset, report.original, report.written,
            report.restored.unwrap_or(0),
        );
        Ok(())
    } else {
        anyhow::bail!(
            "write round-trip failed: wrote 0x{:04x}, read back {:?}, restored {:?}",
            report.written,
            report.read_back,
            report.restored
        )
    }
}
