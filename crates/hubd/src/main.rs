//! `hubd` — the resident core process.
//!
//! Responsibilities: own the pipe server, own adapter instances, own the tray
//! icon and the single-instance mutex. It survives the UI being closed and it is
//! the only process that talks to devices.

use std::process::ExitCode;
use std::sync::Arc;

use hub_core::default_config_dir;
use hubd::{server, single, tray, ui, Service};
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
    if args.iter().any(|arg| arg == "--selftest") {
        return match hubd::selftest::run() {
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
         --selftest   Run the headless acceptance check and exit\n  \
         --version    Print the version and exit\n  \
         --help       Print this help and exit\n\n\
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
