//! `pixmad`: Background USB multiplexer and Unix domain socket daemon for Canon PIXMA G2010.

use clap::Parser;
use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_daemon::server::DaemonServer;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};
use std::path::PathBuf;

const DEFAULT_SYSTEM_STATE_FILE: &str = "/var/lib/pixma-g2010/state.json";

#[derive(Parser, Debug)]
#[command(name = "pixmad", about = "Canon PIXMA G2010 USB multiplexer daemon")]
struct Args {
    /// Unix domain socket path for IPC clients
    #[arg(short, long, default_value = "/run/pixma/pixmad.sock")]
    socket: PathBuf,

    /// Persistent maintenance state file path
    #[arg(long, default_value = DEFAULT_SYSTEM_STATE_FILE)]
    state_file: PathBuf,

    /// Force in-memory virtual hardware emulator (no physical USB required)
    #[arg(long)]
    mock: bool,

    /// Fail instead of falling back to mock if no physical USB device is found
    #[arg(long)]
    require_hardware: bool,
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
    let require_hw =
        args.require_hardware || std::env::var("PIXMA_REQUIRE_USB").as_deref() == Ok("1");

    let transport = if is_mock_forced {
        tracing::info!("Running in mock mode using MockG2010Transport");
        AnyTransport::Mock(Box::new(MockG2010Transport::new()))
    } else {
        let mut opened = None;
        let attempts = if require_hw { 10 } else { 1 };
        for attempt in 1..=attempts {
            if let Ok(devices) = discover_canon_devices() {
                if !devices.is_empty() {
                    match NusbTransport::open_first() {
                        Ok(nusb) => {
                            tracing::info!("Claimed physical Canon G-series USB interface");
                            opened = Some(AnyTransport::Usb(nusb));
                            break;
                        }
                        Err(err) => {
                            if require_hw && attempt < attempts {
                                tracing::debug!(
                                    "USB open attempt {attempt}/{attempts} failed ({err}); retrying..."
                                );
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                continue;
                            }
                            if require_hw {
                                anyhow::bail!("Failed to claim physical USB device: {err}");
                            }
                            tracing::warn!(
                                "Failed to claim physical USB device ({err}); falling back to mock"
                            );
                        }
                    }
                }
            }
            if require_hw && attempt < attempts {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }

        match opened {
            Some(t) => t,
            None => {
                if require_hw {
                    anyhow::bail!("No physical Canon PIXMA USB device detected (--require-hardware)");
                }
                tracing::info!("No physical Canon USB device detected; starting in mock mode");
                AnyTransport::Mock(Box::new(MockG2010Transport::new()))
            }
        }
    };

    let supervisor = match MaintenanceSupervisor::load_or_init(&args.state_file) {
        Ok(sup) => sup,
        Err(err) if err.kind() == std::io::ErrorKind::InvalidData => {
            anyhow::bail!(
                "Refusing to start with corrupted maintenance state file {:?}: {err}. Inspect or remove the corrupted file manually to prevent silent D-counter loss.",
                args.state_file
            );
        }
        Err(err) => {
            let mut resolved = None;
            if args.state_file == std::path::Path::new(DEFAULT_SYSTEM_STATE_FILE) {
                if let Some(user_state) = std::env::var_os("XDG_STATE_HOME")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .filter(|p| p.is_absolute())
                    .map(|p| p.join("pixma-g2010").join("state.json"))
                    .or_else(|| {
                        std::env::var_os("HOME")
                            .filter(|v| !v.is_empty())
                            .map(|h| PathBuf::from(h).join(".local/state/pixma-g2010/state.json"))
                    })
                {
                    match MaintenanceSupervisor::load_or_init(&user_state) {
                        Ok(sup) => {
                            tracing::warn!(
                                "State path {:?} is not writable ({err}); using user state file {:?}",
                                args.state_file,
                                user_state
                            );
                            resolved = Some(sup);
                        }
                        Err(user_err) if user_err.kind() == std::io::ErrorKind::InvalidData => {
                            anyhow::bail!(
                                "Refusing to start with corrupted user maintenance state file {:?}: {user_err}. Inspect or remove the corrupted file manually.",
                                user_state
                            );
                        }
                        Err(_) => {}
                    }
                }
            }
            match resolved {
                Some(sup) => sup,
                None => {
                    tracing::warn!(
                        "Could not initialize writable state file {:?}: {err}; using in-memory state",
                        args.state_file
                    );
                    MaintenanceSupervisor::in_memory(MaintenanceRecord::default())
                }
            }
        }
    };

    let controller = G2010Controller::new(transport, supervisor);

    // Fall back to $XDG_RUNTIME_DIR/pixma or a verified 0700 temp dir if default /run/pixma is not writable
    let effective_socket = if let Some(parent) = args.socket.parent() {
        let dir_writable = MaintenanceSupervisor::probe_dir_writable(parent).is_ok();
        if !dir_writable {
            let fallback_dir = std::env::var_os("XDG_RUNTIME_DIR")
                .filter(|v| !v.is_empty())
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
                .map(|p| p.join("pixma"))
                .unwrap_or_else(|| {
                    std::env::temp_dir().join(format!("pixma-daemon-{}", pixma_daemon::current_uid()))
                });
            pixma_daemon::ensure_verified_private_dir(&fallback_dir).map_err(|e| {
                anyhow::anyhow!("Cannot prepare fallback socket dir {:?}: {e}", fallback_dir)
            })?;
            let fallback = fallback_dir.join("pixmad.sock");
            tracing::warn!(
                "Cannot write to directory {:?}; falling back to {:?}",
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

    #[cfg(unix)]
    let mut sigterm =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;

    tokio::select! {
        res = server.run() => {
            if let Err(e) = res {
                tracing::error!("Server error: {e}");
            }
        }
        _ = tokio::signal::ctrl_c() => {
            tracing::info!("Received SIGINT/Ctrl-C; shutting down pixmad gracefully");
        }
        _ = sigterm.recv() => {
            tracing::info!("Received SIGTERM; shutting down pixmad gracefully");
        }
    }

    Ok(())
}
