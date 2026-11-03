//! Host side of out-of-process adapters: a child process speaking one JSON
//! object per line over stdin/stdout (see `docs/adapter-protocol.md`).
//!
//! The transport is deliberately behind [`LineTransport`] so the protocol
//! behaviour — handshake, matching ids, error mapping, timeouts, the disable
//! latch — is testable without a process or any hardware. [`ChildTransport`] is
//! the real implementation; [`FakeTransport`] is the scripted one used by tests.

use std::collections::VecDeque;
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use hub_core::adapter::{Adapter, AdapterFactory, AdapterInfo, AdapterKind};
use hub_core::device::DeviceDescriptor;
use hub_core::error::{ErrorKind, HubError, Result};
use hub_core::value::CapabilityValue;
use hub_proto::proc::{
    method, EnumerateResult, HelloParams, HelloResult, ReadParams, ReadResult, WriteParams,
    WriteResult, CALL_TIMEOUT_MS, ENUMERATE_TIMEOUT_MS, HANDSHAKE_TIMEOUT_MS,
    MAX_CONSECUTIVE_TIMEOUTS, MAX_LINE_BYTES, PROTOCOL_VERSION,
};
use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};

/// Registered id and display name of the first out-of-process adapter we ship.
pub const ADAPTER_ID: &str = "atk";

/// Descriptor for one out-of-process helper.
///
/// The hub does not care which protocol a helper speaks; it only needs a stable
/// id, a display name and the executable that implements the stdio dialect.
pub fn adapter_info_with(id: &str, name: &str) -> AdapterInfo {
    AdapterInfo {
        id: id.to_string(),
        name: name.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        kind: AdapterKind::OutOfProcess,
    }
}

pub fn adapter_info() -> AdapterInfo {
    adapter_info_with(ADAPTER_ID, "ATK (HID)")
}

/// Factory for [`AdapterInfo`] registrations. Each call builds a fresh adapter,
/// which is how enabling a crashed adapter clears its disabled latch.
pub fn factory_for(info: AdapterInfo, binary: PathBuf) -> AdapterFactory {
    Arc::new(move || Box::new(ProcAdapter::new(info.clone(), binary.clone())))
}

pub fn factory(binary: PathBuf) -> AdapterFactory {
    factory_for(adapter_info(), binary)
}

fn handshake_timeout() -> Duration {
    Duration::from_millis(HANDSHAKE_TIMEOUT_MS)
}

fn enumerate_timeout() -> Duration {
    Duration::from_millis(ENUMERATE_TIMEOUT_MS)
}

fn call_timeout() -> Duration {
    Duration::from_millis(CALL_TIMEOUT_MS)
}

/// One line of the protocol, in either direction.
///
/// `recv` returns `Ok(None)` when the deadline expires without a line; the
/// adapter treats that as a timeout rather than a transport failure, because
/// the child may just be slow (or wedged, which the timeout counter catches).
pub trait LineTransport: Send {
    fn send(&mut self, line: &str) -> io::Result<()>;
    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>>;
}

// ---------------------------------------------------------------------------
// Real child process
// ---------------------------------------------------------------------------

/// A spawned child with its stderr drained on a thread (a full stderr pipe
/// would otherwise block the adapter mid-call).
pub struct ChildTransport {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<io::Result<String>>,
}

impl ChildTransport {
    fn spawn(binary: &Path, adapter_id: &str) -> io::Result<Self> {
        let mut command = Command::new(binary);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // A protocol adapter is a console program; without this a short-lived
        // window flashes for every (re)start.
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }

        let mut child = command.spawn()?;

        // Wire the pipes through `&mut child` before moving the child into the
        // transport: on an I/O failure we still own it and can reap it.
        let (stdin, lines) = match spawn_io(&mut child, adapter_id) {
            Ok(io) => io,
            Err(error) => {
                // Do not leak a child whose pipes we are about to drop.
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        Ok(Self {
            child,
            stdin,
            lines,
        })
    }
}

/// Take the child's pipes and start the stderr/stdout threads.
fn spawn_io(
    child: &mut Child,
    adapter_id: &str,
) -> io::Result<(ChildStdin, Receiver<io::Result<String>>)> {
    let stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("the child has no stdin pipe"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("the child has no stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("the child has no stderr pipe"))?;

    let stderr_id = adapter_id.to_string();
    std::thread::Builder::new()
        .name(format!("{adapter_id}-stderr"))
        .spawn(move || drain_stderr(stderr, &stderr_id))?;

    let (sender, receiver) = mpsc::channel();
    std::thread::Builder::new()
        .name(format!("{adapter_id}-stdout"))
        .spawn(move || drain_stdout(stdout, sender))?;

    Ok((stdin, receiver))
}

impl LineTransport for ChildTransport {
    fn send(&mut self, line: &str) -> io::Result<()> {
        self.stdin.write_all(line.as_bytes())?;
        self.stdin.write_all(b"\n")?;
        self.stdin.flush()
    }

    fn recv(&mut self, timeout: Duration) -> io::Result<Option<String>> {
        match self.lines.recv_timeout(timeout) {
            Ok(Ok(line)) => Ok(Some(line)),
            Ok(Err(error)) => Err(error),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            // The reader thread only ends when stdout does, so this is the
            // child exiting (or closing its stdout), not a timeout.
            Err(RecvTimeoutError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the adapter closed its stdout",
            )),
        }
    }
}

impl Drop for ChildTransport {
    fn drop(&mut self) {
        // Best effort: tell the child to exit, then make sure it is gone. A
        // wedged adapter must not outlive the hub, so we do not wait for a
        // graceful shutdown before killing it.
        let shutdown = r#"{"jsonrpc":"2.0","method":"adapter.shutdown"}"#;
        let _ = writeln!(self.stdin, "{shutdown}");
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn drain_stderr(stderr: std::process::ChildStderr, adapter_id: &str) {
    let reader = BufReader::new(stderr);
    for line in reader.lines().map_while(std::result::Result::ok) {
        tracing::warn!(adapter = %adapter_id, "{}", line);
    }
}

fn drain_stdout(stdout: ChildStdout, sender: Sender<io::Result<String>>) {
    let mut reader = BufReader::new(stdout);
    let mut line = Vec::new();
    loop {
        match read_capped_line(&mut reader, &mut line) {
            Ok(0) => break,
            Ok(_) => {
                let text = String::from_utf8_lossy(&line)
                    .trim_end_matches(['\r', '\n'])
                    .to_string();
                if sender.send(Ok(text)).is_err() {
                    break;
                }
            }
            Err(error) => {
                let _ = sender.send(Err(error));
                break;
            }
        }
    }
}

/// Read one newline-terminated line into `out`, keeping at most
/// `MAX_LINE_BYTES + 1` bytes so an adversarial child cannot grow our memory
/// without bound while we search for the terminator. The extra byte is what
/// lets the protocol layer tell "exactly at the cap" from "over it".
fn read_capped_line<R: BufRead>(reader: &mut R, out: &mut Vec<u8>) -> io::Result<usize> {
    out.clear();
    let cap = MAX_LINE_BYTES + 1;
    loop {
        let (take, newline) = {
            let available = reader.fill_buf()?;
            if available.is_empty() {
                return Ok(out.len());
            }
            let newline = available.iter().position(|&byte| byte == b'\n');
            let take = newline.map_or(available.len(), |index| index + 1);
            let room = cap.saturating_sub(out.len());
            let copy = take.min(room);
            out.extend_from_slice(&available[..copy]);
            (take, newline.is_some())
        };
        reader.consume(take);
        if newline {
            return Ok(out.len());
        }
    }
}

// ---------------------------------------------------------------------------
// Adapter
// ---------------------------------------------------------------------------

enum Backend {
    /// A real child process, restarted on reconnect.
    Child(PathBuf),
    /// A transport supplied by a test; there is no process to kill or respawn.
    Injected,
}

pub struct ProcAdapter {
    info: AdapterInfo,
    backend: Backend,
    transport: Option<Box<dyn LineTransport>>,
    hello: Option<HelloResult>,
    next_id: u64,
    consecutive_timeouts: u32,
    disabled_reason: Option<String>,
    dirty: bool,
}

impl ProcAdapter {
    pub fn new(info: AdapterInfo, binary: impl Into<PathBuf>) -> Self {
        Self {
            info,
            backend: Backend::Child(binary.into()),
            transport: None,
            hello: None,
            next_id: 1,
            consecutive_timeouts: 0,
            disabled_reason: None,
            dirty: false,
        }
    }

    /// Build an adapter over an injected transport, for tests. The transport is
    /// kept across [`reset`](Self::reset) because there is no child to restart.
    pub fn with_transport(info: AdapterInfo, transport: Box<dyn LineTransport>) -> Self {
        Self {
            info,
            backend: Backend::Injected,
            transport: Some(transport),
            hello: None,
            next_id: 1,
            consecutive_timeouts: 0,
            disabled_reason: None,
            dirty: false,
        }
    }

    /// Clear a disabled latch and force a fresh handshake / child process.
    ///
    /// Dropping a real transport closes stdin and kills + reaps the child; the
    /// next call spawns a new one. An injected test transport has no process,
    /// so it is kept and re-handshaken.
    pub fn reset(&mut self) {
        if matches!(self.backend, Backend::Child(_)) {
            self.transport = None;
        }
        self.hello = None;
        self.next_id = 1;
        self.consecutive_timeouts = 0;
        self.disabled_reason = None;
        self.dirty = false;
    }

    pub fn is_disabled(&self) -> bool {
        self.disabled_reason.is_some()
    }

    pub fn disabled_reason(&self) -> Option<&str> {
        self.disabled_reason.as_deref()
    }

    /// True when the child pushed `devices.changed`; the next `enumerate`
    /// clears it.
    pub fn dirty(&self) -> bool {
        self.dirty
    }

    fn disabled_error(&self, reason: &str) -> HubError {
        HubError::unsupported(format!(
            "adapter `{}` is disabled ({reason}); re-enable it from the panel or tray",
            self.info.id
        ))
    }

    fn disable(&mut self, reason: &str) {
        self.disabled_reason = Some(reason.to_string());
        self.hello = None;
        if matches!(self.backend, Backend::Child(_)) {
            // Dropping the transport kills and reaps the wedged child.
            self.transport = None;
        }
        tracing::warn!(
            adapter = %self.info.id,
            reason,
            "adapter disabled until it is enabled again"
        );
    }

    fn on_timeout(&mut self, method: &str, timeout: Duration) -> HubError {
        self.consecutive_timeouts = self.consecutive_timeouts.saturating_add(1);
        if self.consecutive_timeouts >= MAX_CONSECUTIVE_TIMEOUTS {
            let reason = format!(
                "{} consecutive timeouts (last: `{method}` after {} ms)",
                self.consecutive_timeouts,
                timeout.as_millis()
            );
            self.disable(&reason);
            return self.disabled_error(&reason);
        }
        HubError::new(
            ErrorKind::Timeout,
            format!(
                "adapter `{}`: `{method}` did not answer within {} ms ({} consecutive)",
                self.info.id,
                timeout.as_millis(),
                self.consecutive_timeouts
            ),
        )
    }

    fn on_transport_error(&mut self, error: io::Error) -> HubError {
        let reason = format!("the adapter process stopped answering: {error}");
        self.disable(&reason);
        self.disabled_error(&reason)
    }

    fn ensure_connected(&mut self) -> Result<()> {
        if let Some(reason) = &self.disabled_reason {
            return Err(self.disabled_error(reason));
        }
        if self.hello.is_some() && self.transport.is_some() {
            return Ok(());
        }
        self.hello = None;

        if self.transport.is_none() {
            let transport: Box<dyn LineTransport> = match &self.backend {
                Backend::Child(binary) => {
                    let transport =
                        ChildTransport::spawn(binary, &self.info.id).map_err(|error| {
                            HubError::new(
                                ErrorKind::Io,
                                format!(
                                    "cannot start adapter `{}` ({}): {error}",
                                    self.info.id,
                                    binary.display()
                                ),
                            )
                        })?;
                    Box::new(transport)
                }
                Backend::Injected => {
                    return Err(HubError::internal(format!(
                        "adapter `{}` has no transport to reconnect",
                        self.info.id
                    )))
                }
            };
            self.transport = Some(transport);
        }

        let params = HelloParams {
            protocol: PROTOCOL_VERSION,
            hub_version: env!("CARGO_PKG_VERSION").to_string(),
        };
        let value = match self.call(method::HELLO, &params, handshake_timeout()) {
            Ok(value) => value,
            Err(error) => {
                // A refused handshake leaves no usable connection.
                self.transport = None;
                return Err(error);
            }
        };
        let hello: HelloResult = match decode(value, method::HELLO) {
            Ok(hello) => hello,
            Err(error) => {
                self.transport = None;
                return Err(error);
            }
        };
        if hello.protocol != PROTOCOL_VERSION {
            self.transport = None;
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!(
                    "adapter `{}` speaks protocol {}, the hub speaks {PROTOCOL_VERSION}; refusing it",
                    hello.id, hello.protocol
                ),
            ));
        }
        if hello.id != self.info.id {
            self.transport = None;
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!(
                    "adapter at this path identifies as `{}`, but the hub registered it as `{}`; refusing it",
                    hello.id, self.info.id
                ),
            ));
        }
        tracing::info!(
            adapter = %self.info.id,
            name = %hello.name,
            version = %hello.version,
            "out-of-process adapter ready"
        );
        self.hello = Some(hello);
        Ok(())
    }

    fn encode_request<P: Serialize>(&mut self, method: &str, params: &P) -> Result<(u64, String)> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let params = serde_json::to_value(params).map_err(|error| {
            HubError::internal(format!("cannot encode `{method}` params: {error}"))
        })?;
        let frame = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let line = serde_json::to_string(&frame)
            .map_err(|error| HubError::internal(format!("cannot encode `{method}`: {error}")))?;
        if line.len() > MAX_LINE_BYTES {
            return Err(HubError::new(
                ErrorKind::Protocol,
                format!(
                    "request `{method}` is {} bytes, over the {MAX_LINE_BYTES}-byte line cap",
                    line.len()
                ),
            ));
        }
        Ok((id, line))
    }

    fn call<P: Serialize>(&mut self, method: &str, params: &P, timeout: Duration) -> Result<Value> {
        let (expected, line) = self.encode_request(method, params)?;
        let sent = match self.transport.as_mut() {
            Some(transport) => transport.send(&line),
            None => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "not connected to the adapter",
            )),
        };
        if let Err(error) = sent {
            return Err(self.on_transport_error(error));
        }
        self.exchange(expected, timeout, method)
    }

    fn exchange(&mut self, expected: u64, timeout: Duration, method: &str) -> Result<Value> {
        let Some(mut transport) = self.transport.take() else {
            return Err(HubError::internal(format!(
                "adapter `{}`: lost the transport before `{method}`",
                self.info.id
            )));
        };
        let outcome = wait_for_response(
            transport.as_mut(),
            expected,
            timeout,
            &self.info.id,
            &mut self.dirty,
        );
        match outcome {
            Ok(value) => {
                self.transport = Some(transport);
                self.consecutive_timeouts = 0;
                Ok(value)
            }
            Err(WaitError::Remote(error)) => {
                // A well-formed error frame proves the connection is healthy.
                self.transport = Some(transport);
                self.consecutive_timeouts = 0;
                Err(error)
            }
            Err(WaitError::Timeout) => {
                // Keep the child: a slow device is not a dead one, and a
                // response that arrives late is discarded by id.
                self.transport = Some(transport);
                Err(self.on_timeout(method, timeout))
            }
            Err(WaitError::Io(error)) => Err(self.on_transport_error(error)),
            Err(WaitError::Protocol(message)) => {
                // A malformed stream cannot be resynchronised; drop the
                // connection (killing a real child) and let the next call
                // start over.
                Err(HubError::new(ErrorKind::Protocol, message))
            }
        }
    }
}

impl Adapter for ProcAdapter {
    fn info(&self) -> AdapterInfo {
        self.info.clone()
    }

    fn enumerate(&mut self) -> Result<Vec<DeviceDescriptor>> {
        self.ensure_connected()?;
        // Cleared before the call: a `devices.changed` that arrives while this
        // enumeration is in flight must survive, so the next call refreshes
        // again. This flag never reaches the hub by itself — the child's
        // notification is not a device push, and pretending it were would let
        // a stale device set reach clients.
        self.dirty = false;
        let value = self.call(method::ENUMERATE, &json!({}), enumerate_timeout())?;
        let result: EnumerateResult = decode(value, method::ENUMERATE)?;
        Ok(result.devices)
    }

    fn read(&mut self, device: &str, capability: &str) -> Result<CapabilityValue> {
        self.ensure_connected()?;
        let params = ReadParams {
            device: device.to_string(),
            capability: capability.to_string(),
        };
        let value = self.call(method::READ, &params, call_timeout())?;
        let result: ReadResult = decode(value, method::READ)?;
        Ok(result.value)
    }

    fn write(&mut self, device: &str, capability: &str, value: &CapabilityValue) -> Result<()> {
        self.ensure_connected()?;
        let params = WriteParams {
            device: device.to_string(),
            capability: capability.to_string(),
            value: value.clone(),
        };
        let value = self.call(method::WRITE, &params, call_timeout())?;
        let result: WriteResult = decode(value, method::WRITE)?;
        if !result.accepted {
            // The hub reads back after every write, so a false flag is not an
            // error by itself; still, it is the adapter saying it did not take
            // the request and is worth a line in the log.
            tracing::warn!(
                adapter = %self.info.id,
                device,
                capability,
                "adapter reported accepted=false; relying on the read-back"
            );
        }
        Ok(())
    }
}

impl Drop for ProcAdapter {
    fn drop(&mut self) {
        // Dropping the transport is what kills and reaps a real child.
        self.transport = None;
    }
}

// ---------------------------------------------------------------------------
// Frame handling
// ---------------------------------------------------------------------------

#[derive(Debug)]
enum WaitError {
    Timeout,
    Io(io::Error),
    Protocol(String),
    Remote(HubError),
}

#[derive(Debug, serde::Deserialize)]
struct Frame {
    #[serde(default)]
    id: Option<u64>,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Debug, serde::Deserialize)]
struct WireError {
    #[serde(default)]
    code: Option<i64>,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    data: Option<WireErrorData>,
}

#[derive(Debug, serde::Deserialize)]
struct WireErrorData {
    #[serde(default)]
    kind: Option<String>,
    #[serde(default)]
    retryable: Option<bool>,
}

fn wait_for_response(
    transport: &mut dyn LineTransport,
    expected: u64,
    timeout: Duration,
    adapter_id: &str,
    dirty: &mut bool,
) -> std::result::Result<Value, WaitError> {
    let started = Instant::now();
    loop {
        let Some(remaining) = timeout.checked_sub(started.elapsed()) else {
            return Err(WaitError::Timeout);
        };
        if remaining.is_zero() {
            return Err(WaitError::Timeout);
        }

        let line = match transport.recv(remaining) {
            Ok(Some(line)) => line,
            Ok(None) => return Err(WaitError::Timeout),
            Err(error) => return Err(WaitError::Io(error)),
        };

        if line.len() > MAX_LINE_BYTES {
            return Err(WaitError::Protocol(format!(
                "adapter `{adapter_id}` sent a {} byte line, over the {MAX_LINE_BYTES} byte cap; dropping the connection",
                line.len()
            )));
        }

        let frame: Frame = match serde_json::from_str(&line) {
            Ok(frame) => frame,
            Err(error) => {
                return Err(WaitError::Protocol(format!(
                    "adapter `{adapter_id}` sent a line that is not a JSON-RPC frame: {error}"
                )))
            }
        };

        match (frame.id, frame.method.as_deref()) {
            (None, Some(method::DEVICES_CHANGED)) => {
                // The protocol's only push: mark the device set stale. It is
                // still the hub that decides when to re-enumerate.
                *dirty = true;
            }
            (None, Some(other)) => {
                tracing::warn!(adapter = %adapter_id, method = other, "ignoring unknown notification");
            }
            (None, None) => {
                return Err(WaitError::Protocol(format!(
                    "adapter `{adapter_id}` sent a frame without an id or a method"
                )))
            }
            (Some(id), None) => {
                if id < expected {
                    // A late answer to a request we already timed out on.
                    tracing::warn!(adapter = %adapter_id, id, expected, "discarding a stale response");
                    continue;
                }
                if id != expected {
                    return Err(WaitError::Protocol(format!(
                        "adapter `{adapter_id}` answered request {id}, expected {expected}"
                    )));
                }
                if let Some(error) = frame.error {
                    return Err(WaitError::Remote(remote_error(adapter_id, error)));
                }
                return match frame.result {
                    Some(value) => Ok(value),
                    None => Err(WaitError::Protocol(format!(
                        "adapter `{adapter_id}` answered request {id} with neither a result nor an error"
                    ))),
                };
            }
            (Some(_), Some(_)) => {
                return Err(WaitError::Protocol(format!(
                    "adapter `{adapter_id}` sent a request; only responses and notifications are allowed"
                )))
            }
        }
    }
}

fn remote_error(adapter_id: &str, error: WireError) -> HubError {
    let Some(code) = error.code else {
        return HubError::new(
            ErrorKind::Protocol,
            format!("adapter `{adapter_id}` sent an error frame without an integer code"),
        );
    };
    let message = error
        .message
        .unwrap_or_else(|| "the adapter gave no message".to_string());
    let Some(data) = error.data else {
        return HubError::new(
            ErrorKind::Protocol,
            format!("adapter `{adapter_id}` failed with code {code} (`{message}`) and no error.data.kind"),
        );
    };
    let Some(kind) = data.kind.as_deref().and_then(parse_kind) else {
        return HubError::new(
            ErrorKind::Protocol,
            format!(
                "adapter `{adapter_id}` failed with code {code} (`{message}`) and an unknown error.data.kind"
            ),
        );
    };

    let mut text = format!("adapter `{adapter_id}`: {message}");
    let kind = if data.retryable == Some(false) && kind.retryable() {
        // `retryable: false` is binding, but `HubError` has no separate retry
        // flag: the class is what the scene engine consults. Downgrade to the
        // non-retryable class and keep the original one in the message so the
        // log still says what actually failed.
        text.push_str(&format!(
            " (the adapter reported `{}` as not retryable)",
            kind.as_str()
        ));
        ErrorKind::Protocol
    } else {
        kind
    };
    HubError::new(
        kind,
        format!("{text} (code {code}, kind {})", kind.as_str()),
    )
}

fn parse_kind(name: &str) -> Option<ErrorKind> {
    serde_json::from_value(Value::String(name.to_string())).ok()
}

fn decode<T: DeserializeOwned>(value: Value, method: &str) -> Result<T> {
    serde_json::from_value(value).map_err(|error| {
        HubError::new(
            ErrorKind::Protocol,
            format!("the adapter returned a malformed `{method}` result: {error}"),
        )
    })
}

// ---------------------------------------------------------------------------
// Scripted transport for tests
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum Scripted {
    Line(String),
    Timeout,
    Closed,
}

/// A [`LineTransport`] driven by a script instead of a process.
///
/// Replies are consumed in order; an exhausted script behaves like a timeout.
/// Clones share state, so a test can keep a handle for assertions while the
/// adapter owns a boxed clone.
#[derive(Debug, Clone, Default)]
pub struct FakeTransport {
    replies: Arc<Mutex<VecDeque<Scripted>>>,
    sent: Arc<Mutex<Vec<String>>>,
}

impl FakeTransport {
    /// `None` scripts a timeout, `Some(line)` a reply line.
    pub fn new<I, S>(replies: I) -> Self
    where
        I: IntoIterator<Item = Option<S>>,
        S: Into<String>,
    {
        let mut queue = VecDeque::new();
        for reply in replies {
            queue.push_back(match reply {
                Some(line) => Scripted::Line(line.into()),
                None => Scripted::Timeout,
            });
        }
        Self {
            replies: Arc::new(Mutex::new(queue)),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn push(&self, line: impl Into<String>) {
        self.lock_replies().push_back(Scripted::Line(line.into()));
    }

    pub fn push_timeout(&self) {
        self.lock_replies().push_back(Scripted::Timeout);
    }

    /// Make the next `recv` report a closed pipe, as if the child had exited.
    pub fn push_closed(&self) {
        self.lock_replies().push_back(Scripted::Closed);
    }

    /// Lines the adapter wrote, oldest first.
    pub fn sent(&self) -> Vec<String> {
        self.lock_sent().clone()
    }

    /// The sent lines parsed as JSON, for assertions on the wire shape.
    pub fn sent_frames(&self) -> Vec<Value> {
        self.lock_sent()
            .iter()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }

    fn lock_replies(&self) -> std::sync::MutexGuard<'_, VecDeque<Scripted>> {
        match self.replies.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn lock_sent(&self) -> std::sync::MutexGuard<'_, Vec<String>> {
        match self.sent.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

impl LineTransport for FakeTransport {
    fn send(&mut self, line: &str) -> io::Result<()> {
        self.lock_sent().push(line.to_string());
        Ok(())
    }

    fn recv(&mut self, _timeout: Duration) -> io::Result<Option<String>> {
        let next = self.lock_replies().pop_front();
        match next {
            Some(Scripted::Line(line)) => Ok(Some(line)),
            Some(Scripted::Timeout) | None => Ok(None),
            Some(Scripted::Closed) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "the fake adapter closed its pipe",
            )),
        }
    }
}

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn test_info() -> AdapterInfo {
        AdapterInfo {
            id: ADAPTER_ID.to_string(),
            name: "ATK (HID)".to_string(),
            version: "0.1.0".to_string(),
            kind: AdapterKind::OutOfProcess,
        }
    }

    fn hello_line(protocol: u32) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"id": "atk", "name": "ATK", "version": "0.1.0", "protocol": protocol}
        })
        .to_string()
    }

    fn error_line(kind: &str, retryable: bool) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "error": {
                "code": -32000,
                "message": "I2C read failed",
                "data": {"kind": kind, "retryable": retryable}
            }
        })
        .to_string()
    }

    #[test]
    fn every_error_kind_name_round_trips() {
        for kind in [
            ErrorKind::Unsupported,
            ErrorKind::UnknownAdapter,
            ErrorKind::UnknownDevice,
            ErrorKind::UnknownCapability,
            ErrorKind::ReadOnly,
            ErrorKind::InvalidValue,
            ErrorKind::Busy,
            ErrorKind::Timeout,
            ErrorKind::Transient,
            ErrorKind::Protocol,
            ErrorKind::PermissionDenied,
            ErrorKind::Io,
            ErrorKind::Internal,
        ] {
            assert_eq!(parse_kind(kind.as_str()), Some(kind));
        }
        assert_eq!(parse_kind("no-such-kind"), None);
    }

    #[test]
    fn retryable_error_keeps_its_kind() {
        let error = remote_error(
            "atk",
            WireError {
                code: Some(-32000),
                message: Some("I2C read failed".into()),
                data: Some(WireErrorData {
                    kind: Some("transient".into()),
                    retryable: Some(true),
                }),
            },
        );
        assert_eq!(error.kind, ErrorKind::Transient);
        assert!(error.kind.retryable());
        assert!(error.message.contains("I2C read failed"));
    }

    #[test]
    fn not_retryable_downgrades_a_retryable_kind() {
        let error = remote_error(
            "atk",
            WireError {
                code: Some(-32000),
                message: Some("bus is stuck".into()),
                data: Some(WireErrorData {
                    kind: Some("busy".into()),
                    retryable: Some(false),
                }),
            },
        );
        assert!(
            !error.kind.retryable(),
            "retryable=false is binding: {:?}",
            error.kind
        );
        assert!(error.message.contains("bus is stuck"));
        assert!(error.message.contains("not retryable"));
    }

    #[test]
    fn missing_error_data_becomes_protocol() {
        let error = remote_error(
            "atk",
            WireError {
                code: Some(-32000),
                message: Some("boom".into()),
                data: None,
            },
        );
        assert_eq!(error.kind, ErrorKind::Protocol);
    }

    #[test]
    fn unknown_error_kind_becomes_protocol() {
        let error = remote_error(
            "atk",
            WireError {
                code: Some(-32000),
                message: Some("boom".into()),
                data: Some(WireErrorData {
                    kind: Some("mystery".into()),
                    retryable: Some(true),
                }),
            },
        );
        assert_eq!(error.kind, ErrorKind::Protocol);
    }

    #[test]
    fn oversize_lines_are_truncated_at_the_cap() {
        let mut input = vec![b'a'; MAX_LINE_BYTES + 10];
        input.push(b'\n');
        input.extend_from_slice(b"{\"ok\":true}\n");
        let mut reader = BufReader::new(std::io::Cursor::new(input));
        let mut line = Vec::new();

        let first = read_capped_line(&mut reader, &mut line).expect("first line");
        assert_eq!(first, MAX_LINE_BYTES + 1, "only cap + 1 bytes are kept");
        let first = String::from_utf8_lossy(&line).to_string();
        assert!(first.len() > MAX_LINE_BYTES);

        let second = read_capped_line(&mut reader, &mut line).expect("second line");
        assert_eq!(
            String::from_utf8_lossy(&line[..second]).trim(),
            "{\"ok\":true}"
        );
    }

    #[test]
    fn handshake_rejects_a_protocol_mismatch() {
        let fake = FakeTransport::new([Some(hello_line(PROTOCOL_VERSION + 1))]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let error = adapter.enumerate().expect_err("mismatch is refused");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.message.contains("protocol"));
    }

    #[test]
    fn handshake_rejects_an_unknown_adapter_id() {
        let hello = json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {"id": "someone-else", "name": "X", "version": "1", "protocol": PROTOCOL_VERSION}
        })
        .to_string();
        let fake = FakeTransport::new([Some(hello)]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let error = adapter.enumerate().expect_err("wrong id is refused");
        assert_eq!(error.kind, ErrorKind::Protocol);
        assert!(error.message.contains("someone-else"));
    }

    #[test]
    fn sent_requests_are_numbered_and_well_formed() {
        let fake = FakeTransport::new([
            Some(hello_line(PROTOCOL_VERSION)),
            Some(json!({"jsonrpc": "2.0", "id": 2, "result": {"devices": []}}).to_string()),
        ]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake.clone()));
        adapter.enumerate().expect("enumerate");
        let frames = fake.sent_frames();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["method"], method::HELLO);
        assert_eq!(frames[0]["id"], 1);
        assert_eq!(frames[0]["params"]["protocol"], PROTOCOL_VERSION);
        assert_eq!(frames[1]["method"], method::ENUMERATE);
        assert_eq!(frames[1]["id"], 2);
    }

    #[test]
    fn read_maps_the_wire_value() {
        let fake = FakeTransport::new([
            Some(hello_line(PROTOCOL_VERSION)),
            Some(
                json!({"jsonrpc": "2.0", "id": 2, "result": {"value": {"type": "int", "value": 42}}})
                    .to_string(),
            ),
        ]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let value = adapter.read("atk:mouse-1", "battery").expect("read");
        assert_eq!(value, CapabilityValue::Int(42));
    }

    #[test]
    fn error_frames_map_to_typed_errors() {
        let fake = FakeTransport::new([
            Some(hello_line(PROTOCOL_VERSION)),
            Some(error_line("transient", true)),
        ]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let error = adapter.read("atk:mouse-1", "battery").expect_err("error");
        assert_eq!(error.kind, ErrorKind::Transient);
        assert!(error.kind.retryable());
    }

    #[test]
    fn a_timeout_returns_timeout() {
        let fake = FakeTransport::new([Some(hello_line(PROTOCOL_VERSION)), None]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let error = adapter.read("atk:mouse-1", "battery").expect_err("timeout");
        assert_eq!(error.kind, ErrorKind::Timeout);
        assert!(!adapter.is_disabled());
    }

    #[test]
    fn three_consecutive_timeouts_disable_and_reset_reenables() {
        let fake = FakeTransport::new([
            Some(hello_line(PROTOCOL_VERSION)),
            None,
            None,
            None,
            Some(hello_line(PROTOCOL_VERSION)),
            Some(json!({"jsonrpc": "2.0", "id": 2, "result": {"devices": []}}).to_string()),
        ]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));

        for _ in 0..2 {
            let error = adapter.read("atk:mouse-1", "battery").expect_err("timeout");
            assert_eq!(error.kind, ErrorKind::Timeout);
            assert!(!adapter.is_disabled());
        }
        let error = adapter.read("atk:mouse-1", "battery").expect_err("third");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("atk"));
        assert!(adapter.is_disabled());

        // A disabled adapter answers without touching the transport.
        let error = adapter
            .read("atk:mouse-1", "battery")
            .expect_err("disabled");
        assert_eq!(error.kind, ErrorKind::Unsupported);

        adapter.reset();
        assert!(!adapter.is_disabled());
        let devices = adapter.enumerate().expect("re-enumerated after reset");
        assert!(devices.is_empty());
    }

    #[test]
    fn devices_changed_sets_dirty_and_enumerate_clears_it() {
        let fake = FakeTransport::new([
            Some(hello_line(PROTOCOL_VERSION)),
            Some(json!({"jsonrpc": "2.0", "method": "devices.changed", "params": {}}).to_string()),
            Some(json!({"jsonrpc": "2.0", "id": 2, "result": {"devices": []}}).to_string()),
            Some(json!({"jsonrpc": "2.0", "id": 3, "result": {"devices": []}}).to_string()),
        ]);
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        adapter.enumerate().expect("first enumerate");
        assert!(adapter.dirty(), "the notification arrived while waiting");
        adapter.enumerate().expect("second enumerate");
        assert!(!adapter.dirty(), "the next enumerate clears the flag");
    }

    #[test]
    fn a_closed_pipe_disables_the_adapter() {
        let fake = FakeTransport::new([Some(hello_line(PROTOCOL_VERSION))]);
        fake.push_closed();
        let mut adapter = ProcAdapter::with_transport(test_info(), Box::new(fake));
        let error = adapter.read("atk:mouse-1", "battery").expect_err("exit");
        assert_eq!(error.kind, ErrorKind::Unsupported);
        assert!(error.message.contains("atk"));
        assert!(adapter.is_disabled());
    }
}
