//! `hubd` — the resident core process.
//!
//! Responsibilities: own the pipe server, own adapter instances, own the tray
//! icon and the single-instance mutex. It survives the UI being closed and it is
//! the only process that talks to devices.

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use hub_core::default_config_dir;
use hub_core::Hub;
use hubd::{foreground, foreground::ForegroundWatcher, proc, server, single, tray, ui, Service};
use tokio::runtime::Runtime;

/// Per-session mutex: two interactive sessions may each run a hub, but one
/// session may not run two.
const INSTANCE_MUTEX: &str = "Local\\UniversalPeripheralHub.hubd";

fn main() -> ExitCode {
    init_tracing();
    let args: Vec<String> = std::env::args().skip(1).collect();

    if args.iter().any(|arg| arg == "--help" || arg == "-h") {
        print_help();
        return ExitCode::SUCCESS;
    }

    let adapter = match parse_adapter_flag(&args) {
        Ok(adapter) => adapter,
        Err(message) => {
            eprintln!("hubd: {message}");
            return ExitCode::FAILURE;
        }
    };

    if args.iter().any(|arg| arg == "--selftest") {
        let outcome = match adapter.as_deref() {
            None | Some("mock") => hubd::selftest::run(),
            Some("atk") => selftest_atk(),
            Some(other) => {
                eprintln!(
                    "hubd: unknown --adapter `{other}`; --selftest supports `mock` and `atk`"
                );
                return ExitCode::FAILURE;
            }
        };
        return match outcome {
            Ok(()) => ExitCode::SUCCESS,
            Err(err) => {
                eprintln!("selftest failed: {err:#}");
                ExitCode::FAILURE
            }
        };
    }
    if args.iter().any(|arg| arg == "--version") {
        println!("hubd {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }
    if let Some(adapter) = adapter {
        // Silently ignoring the flag here would make an acceptance run look
        // like it checked something it never touched.
        eprintln!("hubd: --adapter {adapter} is only valid together with --selftest");
        return ExitCode::FAILURE;
    }

    match single::InstanceGuard::acquire(INSTANCE_MUTEX) {
        Ok(Some(guard)) => serve(guard),
        Ok(None) => {
            activate_running_instance();
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("hubd: cannot create the single-instance mutex: {err}");
            ExitCode::FAILURE
        }
    }
}

/// `hubd --selftest --adapter atk`: exercise the real out-of-process path
/// through the same [`Hub`] the resident process uses.
///
/// The hub registers the mock adapter too, so the run also proves the two can
/// coexist. Anything short of a spawnable helper that enumerates at least one
/// device is a failure: this mode exists to gate the helper, and a silent skip
/// would turn a broken build into a green check.
fn selftest_atk() -> Result<()> {
    let binary = hubd::service::find_atk_adapter().context(
        "adapter `atk`: no atk-adapter.exe found; set UPH_ATK_ADAPTER or build adapters/atk",
    )?;

    let info = proc::adapter_info();
    let mut hub = Hub::new();
    hub.register(adapter_mock::adapter_info(), adapter_mock::factory())
        .context("registering the mock adapter")?;
    hub.register(info.clone(), proc::factory(binary.clone()))
        .context("registering the ATK adapter")?;
    hub.load("mock").context("loading the mock adapter")?;
    hub.load("atk").with_context(|| {
        format!(
            "adapter `atk` could not be spawned or failed its handshake ({})",
            binary.display()
        )
    })?;

    let devices: Vec<_> = hub
        .devices()
        .into_iter()
        .filter(|device| device.adapter == info.id)
        .collect();
    if devices.is_empty() {
        bail!(
            "adapter `atk` returned zero devices from {}",
            binary.display()
        );
    }

    println!("atk: {} ({})", info.name, info.id);
    for device in &devices {
        println!("  device {} ({})", device.id, device.name);
        for capability in &device.capabilities {
            // One unreadable capability must not hide the rest of the device;
            // the exit condition is the spawn/enumeration check above.
            match hub.read(&device.id, &capability.id) {
                Ok(value) => println!("    {} ({}): {value:?}", capability.id, capability.label),
                Err(err) => println!(
                    "    {} ({}): <{:?}: {}>",
                    capability.id, capability.label, err.kind, err.message
                ),
            }
        }
    }
    Ok(())
}

/// Parse `--adapter <id>` / `--adapter=<id>`. A missing value is a usage error
/// rather than something to skip, so it is reported before anything runs.
fn parse_adapter_flag(args: &[String]) -> std::result::Result<Option<String>, String> {
    let mut adapter = None;
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--adapter" {
            match iter.next() {
                Some(value) => adapter = Some(value.clone()),
                None => return Err("--adapter needs a value (`--adapter atk`)".to_string()),
            }
        } else if let Some(value) = arg.strip_prefix("--adapter=") {
            if value.is_empty() {
                return Err("--adapter needs a value (`--adapter atk`)".to_string());
            }
            adapter = Some(value.to_string());
        }
    }
    Ok(adapter)
}

fn serve(_guard: single::InstanceGuard) -> ExitCode {
    let config_dir = default_config_dir();
    if let Err(err) = std::fs::create_dir_all(&config_dir) {
        eprintln!(
            "hubd: cannot create {}: {err}; settings will not persist",
            config_dir.display()
        );
    }

    let ui_path = ui::find_ui_executable();
    match &ui_path {
        Some(path) => tracing::info!(path = %path.display(), "UI executable found"),
        None => tracing::warn!("UI executable not found; the tray can still control adapters"),
    }
    let service = Service::bootstrap(config_dir.join("settings.json"), ui_path);

    // Foreground tracking feeds the scene engine. The watcher is optional: a
    // denied hook (locked-down desktop, unusual session) degrades to polling
    // inside the module, and a total failure only means scenes stop switching
    // on their own — the rest of the hub keeps working.
    let _watcher = {
        let watcher_service = Arc::clone(&service);
        match ForegroundWatcher::spawn(move |exe| watcher_service.on_foreground(exe)) {
            Ok(watcher) => {
                // The hook only reports changes. Without this the first app
                // switch would be mistaken for the startup baseline and the
                // scene for it would never be applied.
                match foreground::current_executable() {
                    Some(exe) => service.on_foreground(&exe),
                    None => tracing::warn!("no foreground window yet; baseline deferred"),
                }
                Some(watcher)
            }
            Err(err) => {
                tracing::warn!(%err, "foreground tracking unavailable; scenes will not switch");
                None
            }
        }
    };

    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    match tray::spawn(Arc::clone(&service), shutdown_tx) {
        Ok(()) => tracing::info!("tray icon ready"),
        Err(err) => eprintln!("hubd: tray icon unavailable ({err}); continuing without it"),
    }

    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("hubd: cannot start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };

    let pipe = hub_proto::pipe_name();
    let result = runtime.block_on(server::serve(&pipe, service, shutdown_rx));
    match result {
        Ok(()) => {
            tracing::info!("hubd stopped");
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("hubd: pipe server stopped: {err}");
            ExitCode::FAILURE
        }
    }
}

/// A second launch is a request to show the panel, not an error.
fn activate_running_instance() {
    let runtime = match Runtime::new() {
        Ok(runtime) => runtime,
        Err(err) => {
            eprintln!("hubd: cannot start the async runtime: {err}");
            return;
        }
    };
    let pipe = hub_proto::pipe_name();
    runtime.block_on(async {
        use tokio::io::AsyncWriteExt;
        use tokio::net::windows::named_pipe::ClientOptions;

        let notification =
            hub_proto::Request::notification(hub_proto::method::ACTIVATE, serde_json::Value::Null);
        let mut line = match serde_json::to_vec(&notification) {
            Ok(line) => line,
            Err(err) => {
                eprintln!("hubd: cannot encode the activation request: {err}");
                return;
            }
        };
        line.push(b'\n');

        // The first instance may still be creating the pipe.
        for _ in 0..25 {
            if let Ok(mut client) = ClientOptions::new().open(&pipe) {
                if client.write_all(&line).await.is_ok() && client.flush().await.is_ok() {
                    println!("asked the running hubd to open the panel");
                    return;
                }
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        eprintln!("hubd is already running but did not answer on {pipe}");
    });
}

fn print_help() {
    println!(
        "hubd {}\n\n\
         Usage: hubd [OPTIONS]\n\n\
         Options:\n  \
         --selftest             Run the headless acceptance check and exit\n  \
         --selftest --adapter atk\n                       \
         Check the out-of-process ATK adapter and print every capability value\n  \
         --adapter <id>         With --selftest: `mock` (default) or `atk`\n  \
         --version              Print the version and exit\n  \
         --help                 Print this help and exit\n\n\
         Without options hubd starts the resident core: named pipe, adapters and tray icon.\n\
         Starting it twice asks the running instance to open the panel.",
        env!("CARGO_PKG_VERSION")
    );
}

fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};

    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
