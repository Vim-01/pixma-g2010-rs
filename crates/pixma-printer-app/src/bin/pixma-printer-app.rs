//! IPP Everywhere / CUPS 3.x Printer Application binary for Canon PIXMA G2010.

use anyhow::Result;
use pixma_daemon::DaemonClient;
use pixma_printer_app::router::{build_router, AppState};
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
    let mut force_mock = false;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" | "-b" => {
                if i + 1 < args.len() {
                    bind_str = args[i + 1].clone();
                    i += 1;
                }
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
                println!(
                    "  -m, --mock               Simulate hardware with virtual in-memory device"
                );
                println!("  -h, --help               Show this help message");
                return Ok(());
            }
            _ => {}
        }
        i += 1;
    }

    if force_mock {
        env::set_var("PIXMA_MOCK", "1");
        tracing::info!("Mock mode forced via CLI flag --mock");
    }

    let bind_addr: SocketAddr = bind_str
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address '{bind_str}': {e}"))?;

    let state = if force_mock {
        let client = DaemonClient::connect_or_fallback(None, None).await?;
        AppState::new(client)
    } else {
        tracing::info!("Configured on-demand USB/daemon connection mode (releases USB when idle)");
        AppState::new_on_demand()
    };
    let app = build_router(state);

    tracing::info!("Canon PIXMA G2010 IPP Printer App listening on http://{bind_addr}");
    tracing::info!("IPP 2.0 endpoint available at http://{bind_addr}/ipp/print");
    tracing::info!("REST API available at http://{bind_addr}/api/status");

    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
