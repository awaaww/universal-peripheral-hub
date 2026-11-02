//! Read-only DDC/CI probe: what does this machine actually expose?
//!
//! Run with `cargo run -p adapter-ddc --example probe`. It never writes a VCP
//! code; it only enumerates outputs, reads identity, the capabilities string,
//! a few probe codes and the high-level brightness transport.

use std::time::Instant;

use adapter_ddc::win;

fn timed<T>(label: &str, f: impl FnOnce() -> Result<T, win::DdcError>) -> Option<T> {
    let start = Instant::now();
    match f() {
        Ok(value) => {
            println!("       {label}: ok ({} ms)", start.elapsed().as_millis());
            Some(value)
        }
        Err(err) => {
            println!("       {label}: {err} ({} ms)", start.elapsed().as_millis());
            None
        }
    }
}

fn main() {
    let outputs = win::display_outputs();
    println!("display outputs: {}", outputs.len());

    for output in &outputs {
        println!(
            "\n== {} ({}) primary={} rect={:?}",
            output.display_name, output.adapter_name, output.primary, output.rect
        );
        println!("   device_id : {}", output.device_id);
        println!("   device_key: {}", output.device_key);
        match &output.identity {
            Some(id) => println!(
                "   edid      : {} mfg={} product={:04X} serial={:08X} {}-W{:02}",
                id.key,
                id.manufacturer,
                id.product,
                id.serial,
                id.year_of_manufacture,
                id.week_of_manufacture
            ),
            None => println!("   edid      : <unreadable>"),
        }

        let mut monitors = match win::physical_monitors(output.hmonitor) {
            Ok(monitors) => monitors,
            Err(err) => {
                println!("   physical monitors: {err}");
                continue;
            }
        };
        println!("   physical monitors: {}", monitors.len());

        for (index, monitor) in monitors.iter().enumerate() {
            println!(
                "   [{index}] desc={:?} handle={:#x}",
                monitor.description(),
                monitor.handle
            );

            if let Some((current, maximum)) =
                timed("fast 0x10", || win::read_vcp(monitor.handle, 0x10))
            {
                println!("           value={current} max={maximum}");
            }
            timed("fast 0x02", || win::read_vcp(monitor.handle, 0x02));
            timed("fast 0xDF", || win::read_vcp(monitor.handle, 0xDF));

            if let Some(caps) = timed("capabilities", || win::read_capabilities(monitor.handle)) {
                let preview: String = caps.chars().take(160).collect();
                println!("           len={} head={preview:?}", caps.len());
            }

            if let Some((min, current, max)) = timed("high-level brightness", || {
                win::read_high_level_brightness(monitor.handle)
            }) {
                println!("           min={min} value={current} max={max}");
            }
        }

        win::destroy_physical_monitors(&mut monitors);
    }
}
