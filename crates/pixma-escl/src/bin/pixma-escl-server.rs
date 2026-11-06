//! Executable eSCL (AirScan) microserver for Canon PIXMA G2010.
//!
//! Run with:
//! `cargo run -p pixma-escl --bin pixma-escl-server -- [--bind 127.0.0.1:8080] [--mock]`

use std::sync::Arc;
use tokio::net::TcpListener;

use pixma_escl::server::{create_escl_router, EsclServerState};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let mut bind_addr = "127.0.0.1:8080".to_string();
    let mut mock = false;

    let args: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--bind" => {
                if i + 1 < args.len() {
                    bind_addr = args[i + 1].clone();
                    i += 1;
                }
            }
            "--mock" => {
                mock = true;
            }
            "-h" | "--help" => {
                println!("Canon PIXMA G2010 eSCL (AirScan) Microserver");
                println!("Usage: pixma-escl-server [OPTIONS]");
                println!();
                println!("Options:");
                println!("  --bind <ADDR>   Address to bind HTTP server (default: 127.0.0.1:8080)");
                println!("  --mock          Run using virtual G2010 hardware emulator");
                println!("  -h, --help      Print help information");
                return Ok(());
            }
            other => {
                eprintln!("Unknown argument: {other}. Use --help for usage information.");
            }
        }
        i += 1;
    }

    tracing::info!("Starting eSCL server on {bind_addr} (mock={mock})");

    let state = Arc::new(EsclServerState::new(mock));
    let router = create_escl_router(state);

    let listener = TcpListener::bind(&bind_addr).await?;
    tracing::info!("eSCL server listening on http://{bind_addr}");

    axum::serve(listener, router).await?;

    Ok(())
}
