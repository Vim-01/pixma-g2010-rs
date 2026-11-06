//! Unix domain socket server and request dispatcher for `pixmad`.

use crate::ipc::{
    recv_request, send_response, DaemonRequest, DaemonResponse, DeviceArbitrator, JobLockKind,
};
use pixma_core::errors::DriverError;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::UsbTransport;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

/// Server listening on a local Unix domain socket and brokering commands to the G2010 controller.
pub struct DaemonServer {
    socket_path: PathBuf,
    listener: UnixListener,
    controller: Arc<Mutex<G2010Controller<AnyTransport>>>,
    arbitrator: Arc<DeviceArbitrator>,
}

impl DaemonServer {
    /// Binds to the specified socket path, cleaning up any stale socket files.
    pub fn bind<P: AsRef<Path>>(
        socket_path: P,
        controller: G2010Controller<AnyTransport>,
    ) -> std::io::Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();

        if socket_path.exists() {
            let _ = std::fs::remove_file(&socket_path);
        }

        if let Some(parent) = socket_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }

        let listener = UnixListener::bind(&socket_path)?;
        let arbitrator = DeviceArbitrator::new();
        let controller = Arc::new(Mutex::new(controller));

        Ok(Self {
            socket_path,
            listener,
            controller,
            arbitrator,
        })
    }

    /// Returns the socket path this server is bound to.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Serves client connections until an I/O error occurs.
    pub async fn run(&self) -> std::io::Result<()> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let controller = Arc::clone(&self.controller);
            let arbitrator = Arc::clone(&self.arbitrator);

            tokio::spawn(async move {
                if let Err(err) = Self::handle_client(stream, controller, arbitrator).await {
                    tracing::debug!("Client connection closed: {err}");
                }
            });
        }
    }

    /// Handles a single client connection session.
    async fn handle_client(
        mut stream: UnixStream,
        controller: Arc<Mutex<G2010Controller<AnyTransport>>>,
        arbitrator: Arc<DeviceArbitrator>,
    ) -> std::io::Result<()> {
        let (mut reader, mut writer) = stream.split();

        loop {
            let req = match recv_request(&mut reader).await {
                Ok(r) => r,
                Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
                Err(e) => return Err(e),
            };

            let resp = match req {
                DaemonRequest::Ping => DaemonResponse::Pong,
                DaemonRequest::GetStatus => {
                    let _guard = arbitrator.acquire(JobLockKind::Telemetry).await;
                    let mut ctrl = controller.lock().await;
                    match ctrl.query_status().await {
                        Ok(status) => DaemonResponse::Status(status),
                        Err(e) => DaemonResponse::Error(e.to_string()),
                    }
                }
                DaemonRequest::ExecuteMaintenance {
                    action,
                    confirmation_token,
                    override_cooldown,
                } => {
                    let _guard = arbitrator.acquire(JobLockKind::Maintenance).await;
                    let mut ctrl = controller.lock().await;
                    match ctrl
                        .execute_maintenance(
                            action,
                            confirmation_token.as_deref(),
                            override_cooldown,
                        )
                        .await
                    {
                        Ok(status) => DaemonResponse::MaintenanceResult(Ok(status)),
                        Err(DriverError::SafetyInterlock(e)) => {
                            DaemonResponse::MaintenanceResult(Err(e.to_string()))
                        }
                        Err(e) => DaemonResponse::MaintenanceResult(Err(e.to_string())),
                    }
                }
                DaemonRequest::PrintRawStream { data } => {
                    let _guard = arbitrator.acquire(JobLockKind::Printing).await;
                    let mut ctrl = controller.lock().await;
                    const CHUNK_SIZE: usize = 256 * 1024;
                    let mut print_err = None;
                    for chunk in data.chunks(CHUNK_SIZE) {
                        if let Err(e) = ctrl.transport_mut().write_printer(chunk).await {
                            print_err = Some(e.to_string());
                            break;
                        }
                    }
                    if let Some(err) = print_err {
                        DaemonResponse::PrintResult(Err(err))
                    } else {
                        DaemonResponse::PrintResult(Ok(()))
                    }
                }
                DaemonRequest::AcquireScan { params } => {
                    let _guard = arbitrator.acquire(JobLockKind::Scanning).await;
                    let mut ctrl = controller.lock().await;
                    match ctrl.acquire_raw_scan(&params).await {
                        Ok(bytes) => DaemonResponse::ScanData(Ok(bytes)),
                        Err(e) => DaemonResponse::ScanData(Err(e.to_string())),
                    }
                }
            };

            send_response(&mut writer, &resp).await?;
        }

        Ok(())
    }
}

impl Drop for DaemonServer {
    fn drop(&mut self) {
        if self.socket_path.exists() {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }
}
