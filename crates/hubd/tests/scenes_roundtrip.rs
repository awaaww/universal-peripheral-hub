//! Scenes over the real pipe: the document round-trips, `map` re-applies for
//! the current executable, `scene.apply` writes through the engine, and the
//! panel is told about it without asking.
//!
//! The engine's rules themselves (baseline, read-before-write, side effects,
//! retries) are unit-tested in `crates/hubd/src/engine.rs`; this file proves the
//! wire contract and the persistence side effects.

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

/// Read lines until the response carrying `id` arrives. Broadcasts may be
/// interleaved with responses, so matching by id is not optional.
async fn recv_id(
    lines: &mut Lines<BufReader<tokio::io::ReadHalf<NamedPipeClient>>>,
    id: u64,
) -> anyhow::Result<Value> {
    for _ in 0..10 {
        let value = recv(lines).await?;
        if value["id"] == json!(id) {
            return Ok(value);
        }
    }
    anyhow::bail!("no response for id {id}")
}

/// Read lines until a notification with `method` arrives, ignoring responses.
async fn recv_notification(
    lines: &mut Lines<BufReader<tokio::io::ReadHalf<NamedPipeClient>>>,
    wanted: &str,
) -> anyhow::Result<Value> {
    for _ in 0..10 {
        let value = recv(lines).await?;
        if value["method"] == json!(wanted) {
            return Ok(value);
        }
    }
    anyhow::bail!("no `{wanted}` notification arrived")
}

fn document(brightness: i64) -> Value {
    json!({
        "version": 1,
        "scenes": {
            "work": {
                "name": "Work",
                "items": [
                    { "cap_type": "display.brightness", "value": { "type": "int", "value": brightness } }
                ]
            }
        },
        "rules": [ { "default": true, "scene": "work" } ],
        "options": { "allow_side_effects": false, "startup_apply": false }
    })
}

#[tokio::test]
async fn scenes_roundtrip_and_apply() -> anyhow::Result<()> {
    let pipe = format!(r"\\.\pipe\uph-scenes-test-{}", std::process::id());
    let dir = std::env::temp_dir().join(format!("uph-scenes-test-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let settings_path = dir.join("settings.json");

    let service = Service::bootstrap_with(
        settings_path.clone(),
        None,
        ServiceOptions {
            ddc: false,
            atk: false,
            ..Default::default()
        },
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

    // A fresh install has a valid, empty document and an engine that has not
    // seen a foreground event yet.
    send(
        &mut write_half,
        &Request::call(1, method::SCENES_GET, Value::Null),
    )
    .await?;
    let got = recv_id(&mut lines, 1).await?;
    assert_eq!(got["id"], json!(1));
    assert_eq!(got["result"]["document"]["scenes"], json!({}));
    assert_eq!(got["result"]["runtime"]["armed"], json!(false));

    // Saving persists the document and does not touch a device.
    send(
        &mut write_half,
        &Request::call(
            2,
            method::SCENES_SET,
            json!({ "document": document(35), "mode": "save" }),
        ),
    )
    .await?;
    let saved = recv_id(&mut lines, 2).await?;
    assert_eq!(saved["id"], json!(2), "{saved}");
    let push = recv_notification(&mut lines, method::SCENES_CHANGED).await?;
    assert_eq!(push["params"]["document"]["scenes"]["work"]["name"], "Work");
    let on_disk: Value = serde_json::from_str(&std::fs::read_to_string(dir.join("scenes.json"))?)?;
    assert_eq!(on_disk["scenes"]["work"]["items"][0]["value"]["value"], 35);

    // Reading a device before applying gives us the value to compare against.
    send(
        &mut write_half,
        &Request::call(
            3,
            method::CAPABILITY_READ,
            json!({ "device": "mock:display-1", "capability": "brightness" }),
        ),
    )
    .await?;
    let before = recv_id(&mut lines, 3).await?;
    let original = before["result"]["value"]["value"].as_i64().unwrap();

    // Applying the scene writes the new value through the normal hub path.
    send(
        &mut write_half,
        &Request::call(4, method::SCENE_APPLY, json!({ "scene": "work" })),
    )
    .await?;
    let applied = recv_id(&mut lines, 4).await?;
    assert_eq!(applied["id"], json!(4), "{applied}");
    assert_eq!(applied["result"]["runtime"]["current_scene"], "work");
    let report = &applied["result"]["runtime"]["last_report"];
    assert_eq!(report["written"], json!(1), "{report}");
    assert_eq!(report["failed"], Value::Null);

    send(
        &mut write_half,
        &Request::call(
            5,
            method::CAPABILITY_READ,
            json!({ "device": "mock:display-1", "capability": "brightness" }),
        ),
    )
    .await?;
    let after = recv_id(&mut lines, 5).await?;
    assert_eq!(after["result"]["value"]["value"], json!(35));

    // Restore the original value so a developer running this locally does not
    // find their mock display dimmed.
    send(
        &mut write_half,
        &Request::call(
            6,
            method::CAPABILITY_WRITE,
            json!({
                "device": "mock:display-1",
                "capability": "brightness",
                "value": { "type": "int", "value": original }
            }),
        ),
    )
    .await?;
    let _ = recv_id(&mut lines, 6).await?;

    // `map` re-evaluates the current executable. The engine has not seen one
    // yet, so nothing is applied and no device is touched.
    send(
        &mut write_half,
        &Request::call(
            7,
            method::SCENES_SET,
            json!({ "document": document(50), "mode": "map" }),
        ),
    )
    .await?;
    let mapped = recv_id(&mut lines, 7).await?;
    assert_eq!(mapped["id"], json!(7), "{mapped}");
    assert_eq!(
        mapped["result"]["runtime"]["last_report"]["written"],
        json!(1),
        "the earlier apply is still reported"
    );

    // An unknown scene is a typed error, not a silent no-op.
    send(
        &mut write_half,
        &Request::call(8, method::SCENE_APPLY, json!({ "scene": "nope" })),
    )
    .await?;
    let failed = recv_id(&mut lines, 8).await?;
    assert_eq!(failed["id"], json!(8));
    assert_eq!(failed["error"]["data"]["kind"], json!("invalid_value"));

    // A document pointing at a missing scene is rejected before it is saved.
    send(
        &mut write_half,
        &Request::call(
            9,
            method::SCENES_SET,
            json!({
                "document": {
                    "version": 1,
                    "scenes": {},
                    "rules": [ { "default": true, "scene": "ghost" } ]
                },
                "mode": "save"
            }),
        ),
    )
    .await?;
    let rejected = recv_id(&mut lines, 9).await?;
    assert_eq!(rejected["id"], json!(9));
    assert_eq!(rejected["error"]["data"]["kind"], json!("invalid_value"));

    shutdown_tx.send(true)?;
    let _ = tokio::time::timeout(TIMEOUT, server).await??;
    Ok(())
}
