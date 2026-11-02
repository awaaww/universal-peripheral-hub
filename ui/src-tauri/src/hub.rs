//! The UI's side of the hub pipe.
//!
//! The frontend never talks to devices directly: every action goes through
//! `hub_request`, which forwards one JSON-RPC call to `hubd` and waits for the
//! matching response. Server pushes (`devices.changed`) are re-emitted as Tauri
//! events, so a device appearing or disappearing updates the panel without any
//! polling.
//!
//! The connection lives on its own thread with a small tokio runtime: hubd's
//! pipe instances are overlapped, and a blocking `File` handle cannot drive
//! them (a synchronous `WriteFile` against an overlapped instance never
//! completes). One thread owns both directions, which also keeps
//! request/response pairing trivial.

use std::io::ErrorKind;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;

use serde::Serialize;
use serde_json::Value;
use tauri::{AppHandle, Emitter};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::windows::named_pipe::ClientOptions;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_ATTEMPTS: usize = 40;

#[derive(Debug, Serialize)]
pub struct HubFailure {
    pub message: String,
    pub kind: Option<String>,
    pub retryable: bool,
}

impl HubFailure {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            kind: None,
            retryable: false,
        }
    }
}

enum Command {
    Call {
        method: String,
        params: Value,
        reply: mpsc::Sender<Result<Value, HubFailure>>,
    },
}

pub struct Client {
    tx: tokio::sync::mpsc::UnboundedSender<Command>,
    closed: Arc<AtomicBool>,
}

impl Client {
    pub fn request(&self, method: &str, params: Value) -> Result<Value, HubFailure> {
        if self.closed.load(Ordering::SeqCst) {
            return Err(HubFailure::new("the hub connection is closed"));
        }
        let (reply, receiver) = mpsc::channel();
        let command = Command::Call {
            method: method.to_string(),
            params,
            reply,
        };
        if self.tx.send(command).is_err() {
            self.closed.store(true, Ordering::SeqCst);
            return Err(HubFailure::new("the hub connection is closed"));
        }
        match receiver.recv_timeout(REQUEST_TIMEOUT) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(HubFailure::new(format!(
                "hubd did not answer `{method}` within {} s",
                REQUEST_TIMEOUT.as_secs()
            ))),
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err(HubFailure::new("hubd closed the connection"))
            }
        }
    }
}

fn slot() -> &'static Mutex<Option<Arc<Client>>> {
    static SLOT: OnceLock<Mutex<Option<Arc<Client>>>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(None))
}

/// Serialises connection attempts. Two panel commands starting at the same
/// time (a rescan and a capability read, say) must not open two pipes: the
/// second connect would knock the first client off the resident hubd.
fn connect_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

static APP: OnceLock<AppHandle> = OnceLock::new();
static SPAWNED: AtomicBool = AtomicBool::new(false);

pub fn set_app(app: AppHandle) {
    let _ = APP.set(app);
}

/// The live client, if the pipe is still open.
pub fn current() -> Option<Arc<Client>> {
    let guard = slot().lock().expect("client slot");
    guard
        .as_ref()
        .filter(|client| !client.closed.load(Ordering::SeqCst))
        .cloned()
}

fn store(client: Arc<Client>) {
    *slot().lock().expect("client slot") = Some(client);
}

/// Connect, launching `hubd` when it is not running. Runs on a worker thread;
/// callers reach it through `spawn_blocking`.
pub fn ensure() -> Result<Arc<Client>, HubFailure> {
    if let Some(client) = current() {
        return Ok(client);
    }
    // One connector at a time; the loser of the race reuses the winner's client.
    let _connecting = connect_lock().lock().expect("connect lock");
    if let Some(client) = current() {
        return Ok(client);
    }
    let mut last = HubFailure::new("hubd is not running");
    for attempt in 0..CONNECT_ATTEMPTS {
        match try_connect() {
            Ok(client) => {
                store(Arc::clone(&client));
                return Ok(client);
            }
            Err(err) => last = err,
        }
        // Only start a resident core once the pipe has stayed absent for a
        // moment: spawning on the first failed connect races the tray's own
        // hubd and makes it forward an activation instead.
        if attempt == 5 {
            let _ = spawn_hubd();
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(last)
}

fn try_connect() -> Result<Arc<Client>, HubFailure> {
    let (ready_tx, ready_rx) = mpsc::channel::<Result<(), HubFailure>>();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<Command>();
    let closed = Arc::new(AtomicBool::new(false));
    let thread_closed = Arc::clone(&closed);

    std::thread::Builder::new()
        .name("hub-pipe".into())
        .spawn(move || run_connection(ready_tx, thread_closed, rx))
        .map_err(|err| HubFailure::new(format!("cannot start the pipe thread: {err}")))?;

    match ready_rx.recv_timeout(HANDSHAKE_TIMEOUT + Duration::from_secs(5)) {
        Ok(Ok(())) => Ok(Arc::new(Client { tx, closed })),
        Ok(Err(err)) => Err(err),
        Err(_) => Err(HubFailure::new("timed out connecting to hubd")),
    }
}

/// Owns one pipe connection until it breaks. Every request is written and its
/// response read here, so no request-id bookkeeping is needed.
fn run_connection(
    ready: mpsc::Sender<Result<(), HubFailure>>,
    closed: Arc<AtomicBool>,
    mut commands: tokio::sync::mpsc::UnboundedReceiver<Command>,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            let _ = ready.send(Err(HubFailure::new(format!(
                "cannot start the pipe runtime: {err}"
            ))));
            closed.store(true, Ordering::SeqCst);
            return;
        }
    };
    runtime.block_on(async move {
        let name = hub_proto::pipe_name();
        let pipe = match ClientOptions::new().open(&name) {
            Ok(pipe) => pipe,
            Err(err) => {
                let _ = ready.send(Err(HubFailure::new(format!("cannot open {name}: {err}"))));
                closed.store(true, Ordering::SeqCst);
                return;
            }
        };
        let (reader, mut writer) = tokio::io::split(pipe);
        let mut lines = BufReader::new(reader);

        let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, async {
            let request = hub_proto::Request::call(1, hub_proto::method::HELLO, Value::Null);
            write_request(&mut writer, &request).await?;
            read_response(&mut lines, 1).await
        })
        .await;

        let hello = match handshake {
            Ok(Ok(value)) => match response_result(value) {
                Ok(result) => result,
                Err(err) => {
                    let _ = ready.send(Err(err));
                    closed.store(true, Ordering::SeqCst);
                    return;
                }
            },
            Ok(Err(err)) => {
                let _ = ready.send(Err(err));
                closed.store(true, Ordering::SeqCst);
                return;
            }
            Err(_) => {
                let _ = ready.send(Err(HubFailure::new("hubd did not answer the handshake")));
                closed.store(true, Ordering::SeqCst);
                return;
            }
        };
        let protocol = hello.get("protocol").and_then(Value::as_u64).unwrap_or(0);
        if protocol != u64::from(hub_proto::PROTOCOL_VERSION) {
            let _ = ready.send(Err(HubFailure::new(format!(
                "hubd speaks protocol {protocol}, this panel speaks {}",
                hub_proto::PROTOCOL_VERSION
            ))));
            closed.store(true, Ordering::SeqCst);
            return;
        }
        let _ = ready.send(Ok(()));

        // One task owns the read side so an unsolicited push is delivered the
        // moment it arrives. Reading only while a response was expected left
        // `devices.changed` sitting in the pipe until the next call, so the
        // panel showed a stale list after an adapter was toggled elsewhere.
        let (line_tx, mut line_rx) = tokio::sync::mpsc::unbounded_channel();
        tokio::spawn(async move {
            loop {
                match read_line(&mut lines).await {
                    Ok(value) => {
                        if line_tx.send(Ok(value)).is_err() {
                            break;
                        }
                    }
                    Err(err) => {
                        let _ = line_tx.send(Err(err));
                        break;
                    }
                }
            }
        });

        let mut next_id = 2u64;
        let mut pending: Option<(u64, mpsc::Sender<Result<Value, HubFailure>>)> = None;

        loop {
            tokio::select! {
                // Only one request is in flight at a time: the server answers in
                // order, and a second outstanding request would only complicate
                // the bookkeeping. Commands wait in the channel.
                command = commands.recv(), if pending.is_none() => {
                    let Some(Command::Call { method, params, reply }) = command else {
                        break;
                    };
                    let id = next_id;
                    next_id += 1;
                    let request = hub_proto::Request::call(id, &method, params);
                    if let Err(err) = write_request(&mut writer, &request).await {
                        let _ = reply.send(Err(err));
                        break;
                    }
                    pending = Some((id, reply));
                }
                line = line_rx.recv() => {
                    let Some(line) = line else { break };
                    match line {
                        Ok(value) => {
                            let wanted = pending.as_ref().map(|(id, _)| *id);
                            if wanted.is_some() && value.get("id").and_then(Value::as_u64) == wanted {
                                if let Some((_, reply)) = pending.take() {
                                    let _ = reply.send(response_result(value));
                                }
                            } else {
                                forward_notice(&value);
                            }
                        }
                        Err(err) => {
                            if let Some((_, reply)) = pending.take() {
                                let _ = reply.send(Err(err));
                            }
                            break;
                        }
                    }
                }
            }
        }
        closed.store(true, Ordering::SeqCst);
    });
}

async fn write_request<W>(writer: &mut W, request: &hub_proto::Request) -> Result<(), HubFailure>
where
    W: AsyncWriteExt + Unpin,
{
    let mut bytes = serde_json::to_vec(request)
        .map_err(|err| HubFailure::new(format!("cannot encode the request: {err}")))?;
    bytes.push(b'\n');
    writer
        .write_all(&bytes)
        .await
        .map_err(|err| HubFailure::new(format!("cannot send to hubd: {err}")))?;
    writer
        .flush()
        .await
        .map_err(|err| HubFailure::new(format!("cannot flush to hubd: {err}")))
}

/// Reads one JSON object, skipping blank or malformed lines.
async fn read_line<R>(reader: &mut R) -> Result<Value, HubFailure>
where
    R: AsyncBufReadExt + Unpin,
{
    loop {
        let mut line = String::new();
        match reader.read_line(&mut line).await {
            Ok(0) => return Err(HubFailure::new("hubd closed the connection")),
            Ok(_) => {}
            Err(err) => return Err(HubFailure::new(format!("cannot read from hubd: {err}"))),
        }
        if let Ok(value) = serde_json::from_str::<Value>(&line) {
            return Ok(value);
        }
    }
}

/// Reads lines until the response with `id` arrives, forwarding notices.
async fn read_response<R>(reader: &mut R, id: u64) -> Result<Value, HubFailure>
where
    R: AsyncBufReadExt + Unpin,
{
    loop {
        let value = read_line(reader).await?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            return Ok(value);
        }
        forward_notice(&value);
    }
}

fn forward_notice(value: &Value) {
    if value.get("method").and_then(Value::as_str) == Some(hub_proto::method::DEVICES_CHANGED) {
        let payload = value.get("params").cloned().unwrap_or(Value::Null);
        if let Some(app) = APP.get() {
            let _ = app.emit("hub://devices-changed", payload);
        }
    }
}

fn response_result(response: Value) -> Result<Value, HubFailure> {
    if let Some(error) = response.get("error") {
        let data = error.get("data");
        return Err(HubFailure {
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("hub error")
                .to_string(),
            kind: data
                .and_then(|data| data.get("kind"))
                .and_then(Value::as_str)
                .map(str::to_string),
            retryable: data
                .and_then(|data| data.get("retryable"))
                .and_then(Value::as_bool)
                .unwrap_or(false),
        });
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

fn spawn_hubd() -> std::io::Result<()> {
    if SPAWNED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    let Some(path) = find_hubd() else {
        return Err(std::io::Error::new(
            ErrorKind::NotFound,
            "hubd.exe was not found next to the panel",
        ));
    };
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    std::process::Command::new(path)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map(|_| ())
}

fn find_hubd() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("UPH_HUBD_EXE") {
        let path = PathBuf::from(explicit);
        if path.is_file() {
            return Some(path);
        }
    }
    let exe = std::env::current_exe().ok()?;
    let mut dir = exe.parent()?.to_path_buf();
    for _ in 0..6 {
        for candidate in [
            dir.join("hubd.exe"),
            dir.join("target").join("debug").join("hubd.exe"),
            dir.join("target").join("release").join("hubd.exe"),
        ] {
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

#[tauri::command]
pub async fn hub_request(method: String, params: Value) -> Result<Value, HubFailure> {
    tauri::async_runtime::spawn_blocking(move || {
        let client = ensure()?;
        client.request(&method, params)
    })
    .await
    .map_err(|err| HubFailure::new(format!("worker task failed: {err}")))?
}

#[tauri::command]
pub fn hub_status() -> Value {
    serde_json::json!({
        "connected": current().is_some(),
        "protocol": hub_proto::PROTOCOL_VERSION,
    })
}
