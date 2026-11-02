//! Host-side behaviour of the out-of-process adapter, over a scripted transport.
//!
//! Nothing here spawns a process or needs the ATK helper or any hardware: the
//! protocol contract the hub owns — handshake, id matching, error mapping, the
//! timeout latch — is exactly what [`FakeTransport`] can drive. The real child
//! path is what `hubd --selftest --adapter atk` covers on a machine where the
//! helper is installed.

use hub_core::adapter::Adapter;
use hub_core::{CapabilityValue, ErrorKind};
use hubd::proc::{adapter_info, FakeTransport, ProcAdapter};
use serde_json::json;

const PROTOCOL: u32 = hub_proto::proc::PROTOCOL_VERSION;

fn hello(adapter: &str, protocol: u32) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {"id": adapter, "name": "ATK", "version": "0.1.0", "protocol": protocol}
    })
    .to_string()
}

fn devices_reply(id: u64) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {"devices": [{
            "id": "atk:keyboard-1",
            "adapter": "atk",
            "name": "ATK keyboard",
            "capabilities": [{
                "id": "backlight",
                "cap_type": "hid.backlight",
                "label": "Backlight",
                "kind": {"type": "int", "min": 0, "max": 3, "step": 1}
            }]
        }]}
    })
    .to_string()
}

fn adapter_with(script: Vec<Option<String>>) -> (ProcAdapter, FakeTransport) {
    let fake = FakeTransport::new(script);
    let adapter = ProcAdapter::with_transport(adapter_info(), Box::new(fake.clone()));
    (adapter, fake)
}

fn error_reply(id: u64, kind: &str, retryable: bool) -> String {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32000,
            "message": "the HID collection is gone",
            "data": {"kind": kind, "retryable": retryable}
        }
    })
    .to_string()
}

#[test]
fn handshake_succeeds_and_enumerate_returns_devices() {
    let (mut adapter, fake) =
        adapter_with(vec![Some(hello("atk", PROTOCOL)), Some(devices_reply(2))]);

    let devices = adapter.enumerate().expect("handshake + enumerate");
    assert_eq!(devices.len(), 1);
    assert_eq!(devices[0].id, "atk:keyboard-1");
    assert_eq!(devices[0].adapter, "atk");

    let frames = fake.sent_frames();
    assert_eq!(frames[0]["method"], "adapter.hello");
    assert_eq!(frames[0]["params"]["protocol"], PROTOCOL);
    assert_eq!(frames[1]["method"], "adapter.enumerate");
    assert_eq!(frames[1]["id"], 2, "request ids increment per call");
}

#[test]
fn protocol_mismatch_is_rejected() {
    let (mut adapter, _) = adapter_with(vec![Some(hello("atk", PROTOCOL + 1))]);
    let error = adapter.enumerate().expect_err("wrong protocol");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("protocol"));
}

#[test]
fn unknown_adapter_id_is_rejected() {
    let (mut adapter, _) = adapter_with(vec![Some(hello("not-atk", PROTOCOL))]);
    let error = adapter.enumerate().expect_err("wrong id");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(error.message.contains("not-atk"));
}

#[test]
fn read_maps_the_returned_value() {
    let (mut adapter, _) = adapter_with(vec![
        Some(hello("atk", PROTOCOL)),
        Some(
            json!({"jsonrpc": "2.0", "id": 2, "result": {"value": {"type": "int", "value": 80}}})
                .to_string(),
        ),
    ]);
    let value = adapter.read("atk:keyboard-1", "backlight").expect("read");
    assert_eq!(value, CapabilityValue::Int(80));
}

#[test]
fn errors_keep_their_kind_and_retryability() {
    let (mut adapter, _) = adapter_with(vec![
        Some(hello("atk", PROTOCOL)),
        Some(error_reply(2, "transient", true)),
    ]);
    let error = adapter
        .read("atk:keyboard-1", "backlight")
        .expect_err("error");
    assert_eq!(error.kind, ErrorKind::Transient);
    assert!(error.kind.retryable());

    // `retryable: false` is binding: the class is downgraded so nothing retries
    // it, while the message still names what actually failed.
    let (mut adapter, _) = adapter_with(vec![
        Some(hello("atk", PROTOCOL)),
        Some(error_reply(2, "busy", false)),
    ]);
    let error = adapter
        .read("atk:keyboard-1", "backlight")
        .expect_err("error");
    assert_eq!(error.kind, ErrorKind::Protocol);
    assert!(!error.kind.retryable());
    assert!(error.message.contains("not retryable"));
}

#[test]
fn a_timeout_is_a_timeout_and_not_a_disable() {
    let (mut adapter, _) = adapter_with(vec![Some(hello("atk", PROTOCOL)), None]);
    let error = adapter
        .read("atk:keyboard-1", "backlight")
        .expect_err("timeout");
    assert_eq!(error.kind, ErrorKind::Timeout);
    assert!(!adapter.is_disabled(), "one timeout must not disable");
}

#[test]
fn three_timeouts_disable_and_reset_reenables() {
    let (mut adapter, _) = adapter_with(vec![
        Some(hello("atk", PROTOCOL)),
        None,
        None,
        None,
        Some(hello("atk", PROTOCOL)),
        Some(devices_reply(2)),
    ]);

    for _ in 0..2 {
        let error = adapter
            .read("atk:keyboard-1", "backlight")
            .expect_err("timeout");
        assert_eq!(error.kind, ErrorKind::Timeout);
        assert!(!adapter.is_disabled());
    }
    let error = adapter
        .read("atk:keyboard-1", "backlight")
        .expect_err("third");
    assert_eq!(error.kind, ErrorKind::Unsupported);
    assert!(error.message.contains("atk"));
    assert!(adapter.is_disabled());

    // A disabled adapter answers locally, without touching the transport.
    let error = adapter
        .read("atk:keyboard-1", "backlight")
        .expect_err("latched");
    assert_eq!(error.kind, ErrorKind::Unsupported);

    // `reset` is the tray/panel enable path: it must handshake again and work.
    adapter.reset();
    assert!(!adapter.is_disabled());
    let devices = adapter.enumerate().expect("fresh handshake after reset");
    assert_eq!(devices.len(), 1);
}
