//! The pipe is the contract between the UI and the resident core: prove the
//! framing, the RPC methods, the write read-back and the push notification on a
//! real named pipe (no mocks of the transport itself).

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
async fn pipe_round_trip_and_device_push() -> anyhow::Result<()> {
    let pipe = format!(r"\\.\pipe\uph-test-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("uph-pipe-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;

    let service = Service::bootstrap_with(
        dir.join("settings.json"),
        None,
        ServiceOptions { ddc: false },
    );
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let server_pipe = pipe.clone();
    let server = tokio::spawn(async move { serve(&server_pipe, service, shutdown_rx).await });

    let mut client = None;
    for _ in 0..100 {
        match ClientOptions::new().open(&pipe) {
            Ok(opened) => {
                client = Some(opened);
                break;
            }
            Err(_) => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    let client = client.expect("server did not create the pipe");
    let (read_half, mut write_half) = tokio::io::split(client);
    let mut lines = BufReader::new(read_half).lines();

    send(
        &mut write_half,
        &Request::call(1, method::HELLO, Value::Null),
    )
    .await?;
    let hello = recv(&mut lines).await?;
    assert_eq!(hello["id"], json!(1));
    assert_eq!(hello["result"]["hub"], json!("hubd"));
    assert_eq!(hello["result"]["protocol"], json!(1));

    send(
        &mut write_half,
        &Request::call(2, method::DEVICES_LIST, Value::Null),
    )
    .await?;
    let listed = recv(&mut lines).await?;
    assert_eq!(listed["id"], json!(2));
    let devices = listed["result"]["devices"]
        .as_array()
        .expect("devices array");
    assert_eq!(devices.len(), 2, "mock adapter exposes two devices");
    assert_eq!(listed["result"]["adapters"][0]["enabled"], json!(true));

    send(
        &mut write_half,
        &Request::call(
            3,
            method::CAPABILITY_WRITE,
            json!({"device": "mock:display-1", "capability": "brightness", "value": {"type": "int", "value": 41}}),
        ),
    )
    .await?;
    let written = recv(&mut lines).await?;
    assert_eq!(written["result"]["matched"], json!(true));
    assert_eq!(
        written["result"]["value"],
        json!({"type": "int", "value": 41})
    );

    send(
        &mut write_half,
        &Request::call(
            4,
            method::CAPABILITY_READ,
            json!({"device": "mock:display-1", "capability": "brightness"}),
        ),
    )
    .await?;
    let read_back = recv(&mut lines).await?;
    assert_eq!(
        read_back["result"]["value"],
        json!({"type": "int", "value": 41})
    );

    // Blocking is adapter-specific: the mock adapter has no block list, and the
    // error says so instead of silently doing nothing.
    send(
        &mut write_half,
        &Request::call(
            40,
            method::DEVICE_SET_BLOCKED,
            json!({"device": "mock:display-1", "blocked": true}),
        ),
    )
    .await?;
    let unsupported = recv(&mut lines).await?;
    assert_eq!(unsupported["id"], json!(40));
    assert_eq!(unsupported["error"]["code"], json!(-32000));
    assert_eq!(unsupported["error"]["data"]["kind"], json!("unsupported"));

    send(
        &mut write_half,
        &Request::call(
            5,
            method::ADAPTER_SET_ENABLED,
            json!({"adapter": "mock", "enabled": false}),
        ),
    )
    .await?;
    let disabled = recv(&mut lines).await?;
    assert_eq!(disabled["id"], json!(5));
    assert_eq!(disabled["result"]["devices"], json!([]));

    // The server must push the new device set without being asked.
    let pushed = recv(&mut lines).await?;
    assert_eq!(pushed["method"], json!(method::DEVICES_CHANGED));
    assert!(pushed.get("id").is_none(), "notifications carry no id");
    assert_eq!(pushed["params"]["devices"], json!([]));

    // A disabled adapter's devices are unknown, not merely unreadable.
    send(
        &mut write_half,
        &Request::call(
            6,
            method::CAPABILITY_READ,
            json!({"device": "mock:keyboard-1", "capability": "battery"}),
        ),
    )
    .await?;
    let error = recv(&mut lines).await?;
    assert_eq!(error["error"]["code"], json!(-32000));
    assert_eq!(error["error"]["data"]["kind"], json!("unknown_device"));

    let persisted: Value = serde_json::from_slice(&std::fs::read(dir.join("settings.json"))?)?;
    assert_eq!(persisted["disabled_adapters"], json!(["mock"]));

    shutdown_tx.send(true)?;
    tokio::time::timeout(TIMEOUT, server).await???;
    Ok(())
}
