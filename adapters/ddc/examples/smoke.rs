//! Read/write smoke test against the real monitors.
//!
//! Run it with nothing important on screen: it changes brightness to prove the
//! write path, then puts the original value back.
//!
//! ```text
//! cargo run -p adapter-ddc --example smoke
//! ```

use std::time::Instant;

use adapter_ddc::adapter::DdcAdapter;
use hub_core::adapter::Adapter;
use hub_core::value::CapabilityValue;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut adapter = DdcAdapter::new();

    let started = Instant::now();
    let devices = adapter.enumerate()?;
    println!(
        "enumerate: {} device(s) in {:?}",
        devices.len(),
        started.elapsed()
    );

    for device in &devices {
        println!(
            "\n== {} ({}, {})",
            device.name,
            device.vendor.as_deref().unwrap_or("?"),
            device.model.as_deref().unwrap_or("?")
        );
        for capability in &device.capabilities {
            println!(
                "   {:<14} {:<24} {:?}",
                capability.id, capability.cap_type, capability.kind
            );
        }

        let Ok(CapabilityValue::Int(before)) = adapter.read(&device.id, "brightness") else {
            println!("   brightness: not readable");
            continue;
        };
        println!("   brightness now: {before}");

        let probe = if before >= 50 { before - 5 } else { before + 5 };
        let started = Instant::now();
        adapter.write(&device.id, "brightness", &CapabilityValue::Int(probe))?;
        let write = started.elapsed();
        let after = adapter.read(&device.id, "brightness")?;
        println!("   wrote {probe} in {write:?}, read back {after:?}");

        adapter.write(&device.id, "brightness", &CapabilityValue::Int(before))?;
        let restored = adapter.read(&device.id, "brightness")?;
        println!("   restored to {before}, read back {restored:?}");
    }

    Ok(())
}
