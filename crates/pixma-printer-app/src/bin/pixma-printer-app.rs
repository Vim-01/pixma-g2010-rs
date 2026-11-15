//! IPP Everywhere / CUPS 3.x Printer Application binary for Canon PIXMA G2010.

use anyhow::Result;
use pixma_daemon::DaemonClient;
use pixma_printer_app::router::{build_router, printer_uri_base_from_bind, AppState};
use std::env;
use std::net::SocketAddr;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pixma_printer_app=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    let args: Vec<String> = env::args().collect();
    let mut bind_str = String::from("127.0.0.1:8631");
    let mut socket_path: Option<std::path::PathBuf> = None;
    let mut force_mock = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" | "-b" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("--bind requires an <ADDR:PORT> argument");
                }
                bind_str = args[i + 1].clone();
                i += 1;
            }
            "--socket" => {
                if i + 1 >= args.len() {
                    anyhow::bail!("--socket requires a <PATH> argument");
                }
                socket_path = Some(std::path::PathBuf::from(&args[i + 1]));
                i += 1;
            }
            "--mock" | "-m" => {
                force_mock = true;
            }
            "--help" | "-h" => {
                println!("Canon PIXMA G2010 Driverless IPP Everywhere Printer Application");
                println!();
                println!("Usage: pixma-printer-app [OPTIONS]");
                println!();
                println!("Options:");
                println!("  -b, --bind <ADDR:PORT>   Socket address to listen on (default: 127.0.0.1:8631)");
                println!("      --socket <PATH>      Path to pixmad Unix domain socket");
                println!(
                    "  -m, --mock               Simulate hardware with virtual in-memory device"
                );
                println!("  -h, --help               Show this help message");
                return Ok(());
            }
            other => {
                anyhow::bail!("Unknown argument: {other}. Use --help for usage information.");
            }
        }
        i += 1;
    }

    let bind_addr: SocketAddr = bind_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{bind_str}': {e}"))?;
    let uri_base = printer_uri_base_from_bind(bind_addr);

    let state = if force_mock {
        tracing::info!("Mock mode forced via CLI flag --mock");
        let client = DaemonClient::connect_mock(None)?;
        AppState::new_with_uri(client, uri_base)
    } else {
        tracing::info!("Configured fail-closed live USB/daemon connection mode");
        AppState::new_on_demand_with_socket_and_uri(socket_path, uri_base)
    };
    let app = build_router(state);

    tracing::info!("Canon PIXMA G2010 IPP Printer App listening on http://{bind_addr}");
    tracing::info!("IPP 2.0 endpoint available at http://{bind_addr}/ipp/print");
    tracing::info!("REST API available at http://{bind_addr}/api/status");

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
