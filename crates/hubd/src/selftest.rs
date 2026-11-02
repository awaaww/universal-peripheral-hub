//! Headless acceptance check: `hubd --selftest`.
//!
//! It exercises the real in-process adapter path (enumerate, read, write with
//! read-back, unload, load) without a pipe, a window or a tray icon, so CI can
//! prove the core works on a machine with no peripherals.

use anyhow::{bail, Context, Result};
use hub_core::{CapabilityValue, ErrorKind, Hub};

pub fn run() -> Result<()> {
    let mut hub = Hub::new();
    hub.register(adapter_mock::adapter_info(), adapter_mock::factory())
        .context("registering the mock adapter")?;
    hub.load("mock").context("loading the mock adapter")?;

    let devices = hub.devices();
    if devices.len() != 2 {
        bail!("expected 2 mock devices, got {}", devices.len());
    }
    for device in &devices {
        println!(
            "device {} ({} capabilities) from adapter `{}`",
            device.id,
            device.capabilities.len(),
            device.adapter
        );
    }

    let before = hub
        .read("mock:display-1", "brightness")
        .context("reading brightness")?;
    let CapabilityValue::Int(before_value) = before else {
        bail!("brightness is not an int: {before:?}");
    };
    let requested = CapabilityValue::Int(if before_value == 40 { 41 } else { 40 });

    let outcome = hub
        .write("mock:display-1", "brightness", &requested)
        .context("writing brightness")?;
    if outcome.readback_error.is_some() {
        bail!("read-back failed: {:?}", outcome.readback_error);
    }
    if !outcome.matched || outcome.value != requested {
        bail!(
            "read-back mismatch: requested {requested:?}, got {:?}",
            outcome.value
        );
    }
    let after = hub
        .read("mock:display-1", "brightness")
        .context("re-reading brightness")?;
    if after != requested {
        bail!("value did not persist: expected {requested:?}, got {after:?}");
    }
    hub.write(
        "mock:display-1",
        "brightness",
        &CapabilityValue::Int(before_value),
    )
    .context("restoring brightness")?;

    let unknown = hub.read("mock:display-1", "definitely-not-a-capability");
    match unknown {
        Err(err) if err.kind == ErrorKind::UnknownCapability => {}
        other => bail!("expected UnknownCapability, got {other:?}"),
    }

    hub.unload("mock").context("unloading the mock adapter")?;
    if !hub.devices().is_empty() {
        bail!("devices survived unload: {:?}", hub.devices());
    }
    hub.load("mock").context("re-loading the mock adapter")?;
    if hub.devices().len() != 2 {
        bail!(
            "expected 2 devices after reload, got {}",
            hub.devices().len()
        );
    }

    println!(
        "selftest ok: 2 devices, read-back write, typed error, unload/load round-trip (revision {})",
        hub.revision()
    );
    Ok(())
}
