//! `pixmad`: Background USB multiplexer and Unix domain socket daemon for Canon PIXMA G2010.

use clap::Parser;
use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_daemon::server::DaemonServer;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "pixmad", about = "Canon PIXMA G2010 USB multiplexer daemon")]
struct Args {
    /// Unix domain socket path for IPC clients
    #[arg(short, long, default_value = "/run/pixma/pixmad.sock")]
    socket: PathBuf,

    /// Persistent maintenance state file path
    #[arg(long, default_value = "/var/lib/pixma/maintenance_state.json")]
    state_file: PathBuf,

    /// Force in-memory virtual hardware emulator (no physical USB required)
    #[arg(long)]
    mock: bool,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,pixma_daemon=debug,pixma_transport=debug".into()),
        )
        .init();

    let args = Args::parse();

    tracing::info!("Starting pixmad multiplexer on socket: {:?}", args.socket);

    let is_mock_forced = args.mock || std::env::var("PIXMA_MOCK").as_deref() == Ok("1");

    let transport = if is_mock_forced {
        tracing::info!("Running in mock mode using MockG2010Transport");
        AnyTransport::Mock(Box::new(MockG2010Transport::new()))
    } else {
        match discover_canon_devices() {
            Ok(devices) if !devices.is_empty() => match NusbTransport::open_first() {
                Ok(nusb) => {
                    tracing::info!("Claimed physical Canon G-series USB interface");
                    AnyTransport::Usb(nusb)
                }
                Err(err) => {
                    tracing::warn!(
                        "Failed to claim physical USB device ({err}); falling back to mock"
                    );
                    AnyTransport::Mock(Box::new(MockG2010Transport::new()))
                }
            },
            _ => {
                tracing::info!("No physical Canon USB device detected; starting in mock mode");
                AnyTransport::Mock(Box::new(MockG2010Transport::new()))
            }
        }
    };

    let supervisor = MaintenanceSupervisor::load_or_init(&args.state_file).unwrap_or_else(|err| {
        tracing::warn!(
            "Could not load state file {:?}: {err}; using in-memory state",
            args.state_file
        );
        MaintenanceSupervisor::in_memory(MaintenanceRecord::default())
    });

    let controller = G2010Controller::new(transport, supervisor);

    // Fall back to /tmp if default /run/pixma is not writable
    let effective_socket = if let Some(parent) = args.socket.parent() {
        if std::fs::create_dir_all(parent).is_err() {
            let fallback = PathBuf::from("/tmp/pixmad.sock");
            tracing::warn!(
                "Cannot create directory {:?}; falling back to {:?}",
                parent,
                fallback
            );
            fallback
        } else {
            args.socket
        }
    } else {
        args.socket
    };

    let server = DaemonServer::bind(&effective_socket, controller)?;
    tracing::info!("Daemon listening on {:?}", server.socket_path());

    tokio::select! {
        res = server.run() => {
            if let Err(e) = res {
                tracing::error!("Server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT/Ctrl-C; shutting down pixmad gracefully");
        }
    }

    Ok(())
}
