//! Undo and diagnostics are part of the panel contract: a write must leave a
//! reversible record, the undo must actually reach the device, and the
//! diagnostics snapshot must describe what this process is doing.

use std::time::Duration;

use hub_proto::{method, Request};
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::net::windows::named_pipe::{ClientOptions, NamedPipeClient};
use tokio::sync::watch;

use hubd::service::ServiceOptions;
use hubd::{server::serve, Service};

const TIMEOUT: Duration = Duration::from_secs(5);

async fn send<W: tokio::io::AsyncWrite + Unpin>(
    client: &mut W,
    request: &Request,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec(request)?;
    bytes.push(b'\n');
    client.write_all(&bytes).await?;
    client.flush().await?;
    Ok(())
}

async fn recv(
    lines: &mut Lines<BufReader<tokio::io::ReadHalf<NamedPipeClient>>>,
) -> anyhow::Result<Value> {
    let line = tokio::time::timeout(TIMEOUT, lines.next_line())
        .await??
        .ok_or_else(|| anyhow::anyhow!("the server closed the pipe"))?;
    Ok(serde_json::from_str(&line)?)
}

#[tokio::test]
async fn history_undo_and_diagnostics() -> anyhow::Result<()> {
    let pipe = format!(r"\\.\pipe\uph-history-test-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("uph-history-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;

    let service = Service::bootstrap_with(
        dir.join("settings.json"),
        None,
        ServiceOptions {
            ddc: false,
            atk: false,
            via: false,
            ..Default::default()
        },
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_pipe = pipe.clone();
    let server = tokio::spawn(async move { serve(&server_pipe, service, shutdown_rx).await });

    let mut opened = None;
    for _ in 0..100 {
        match ClientOptions::new().open(&pipe) {
            Ok(client) => {
                opened = Some(client);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    let client = opened.expect("server did not create the pipe");
    let (read_half, mut write_half) = tokio::io::split(client);
    let mut lines = BufReader::new(read_half).lines();

    send(
        &mut write_half,
        &Request::call(1, method::HELLO, Value::Null),
    )
    .await?;
    recv(&mut lines).await?;

    // The mock display starts at 62%; change it so there is something to undo.
    send(
        &mut write_half,
        &Request::call(
            2,
            method::CAPABILITY_WRITE,
            json!({"device": "mock:display-1", "capability": "brightness", "value": {"type": "int", "value": 35}}),
        ),
    )
    .await?;
    let written = recv(&mut lines).await?;
    assert_eq!(
        written["result"]["value"],
        json!({"type": "int", "value": 35})
    );
    assert_eq!(written["result"]["matched"], json!(true));

    send(
        &mut write_half,
        &Request::call(
            3,
            method::DEVICE_HISTORY,
            json!({"device": "mock:display-1"}),
        ),
    )
    .await?;
    let history = recv(&mut lines).await?;
    let entries = history["result"]["entries"]
        .as_array()
        .expect("entries array");
    assert_eq!(entries.len(), 1, "one reversible write so far");
    assert_eq!(entries[0]["capability"], json!("brightness"));
    assert_eq!(entries[0]["cap_type"], json!("display.brightness"));
    assert_eq!(
        entries[0]["value"],
        json!({"type": "int", "value": 62}),
        "the entry stores the value that was replaced"
    );
    assert!(entries[0]["at_ms"].as_u64().unwrap_or(0) > 0);

    send(
        &mut write_half,
        &Request::call(4, method::DEVICE_UNDO, json!({"device": "mock:display-1"})),
    )
    .await?;
    let undone = recv(&mut lines).await?;
    assert_eq!(undone["result"]["capability"], json!("brightness"));
    assert_eq!(
        undone["result"]["value"],
        json!({"type": "int", "value": 62})
    );
    assert_eq!(undone["result"]["matched"], json!(true));

    send(
        &mut write_half,
        &Request::call(
            5,
            method::CAPABILITY_READ,
            json!({"device": "mock:display-1", "capability": "brightness"}),
        ),
    )
    .await?;
    let read_back = recv(&mut lines).await?;
    assert_eq!(
        read_back["result"]["value"],
        json!({"type": "int", "value": 62})
    );

    send(
        &mut write_half,
        &Request::call(
            6,
            method::DEVICE_HISTORY,
            json!({"device": "mock:display-1"}),
        ),
    )
    .await?;
    let drained = recv(&mut lines).await?;
    assert_eq!(drained["result"]["entries"], json!([]));

    // Undoing past the beginning is a typed error, and it lands in diagnostics.
    send(
        &mut write_half,
        &Request::call(7, method::DEVICE_UNDO, json!({"device": "mock:display-1"})),
    )
    .await?;
    let empty_undo = recv(&mut lines).await?;
    assert_eq!(empty_undo["error"]["data"]["kind"], json!("invalid_value"));

    send(
        &mut write_half,
        &Request::call(8, method::DIAGNOSTICS_GET, Value::Null),
    )
    .await?;
    let diagnostics = recv(&mut lines).await?;
    assert_eq!(
        diagnostics["result"]["hub_version"],
        json!(env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(diagnostics["result"]["protocol"], json!(1));
    assert_eq!(
        diagnostics["result"]["pid"],
        json!(std::process::id()),
        "diagnostics describe the process that answers"
    );
    assert!(diagnostics["result"]["config_dir"]
        .as_str()
        .unwrap_or_default()
        .contains("uph-history-test"));
    assert!(diagnostics["result"]["pipe"]
        .as_str()
        .unwrap_or_default()
        .contains("universal-peripheral-hub"));
    let adapters = diagnostics["result"]["adapters"]
        .as_array()
        .expect("adapter list");
    assert!(adapters
        .iter()
        .any(|adapter| adapter["id"] == json!("mock")));
    let errors = diagnostics["result"]["recent_errors"]
        .as_array()
        .expect("error list");
    assert!(
        errors
            .iter()
            .any(|error| error["method"] == json!("device.undo")
                && error["kind"] == json!("invalid_value")),
        "the failed undo must be visible in diagnostics: {errors:?}"
    );

    shutdown_tx.send(true)?;
    tokio::time::timeout(TIMEOUT, server).await???;
    Ok(())
}
