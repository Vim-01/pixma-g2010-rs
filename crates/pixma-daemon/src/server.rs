//! Unix domain socket server and request dispatcher for `pixmad`.

use crate::ipc::{
    current_unix_secs, local_hostname, recv_request, send_response, DaemonRequest, DaemonResponse,
    DeviceArbitrator, JobLockKind, LiveDaemonSnapshot,
};
use pixma_core::errors::DriverError;
use pixma_core::models::UsbHardwareMarker;
use pixma_transport::controller::{extract_ieee1284_tag, AnyTransport, G2010Controller};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, RwLock};

struct SharedDaemonMeta {
    daemon_hostname: String,
    hw_marker: UsbHardwareMarker,
    cached_ieee1284: Mutex<Option<String>>,
    last_snapshot: RwLock<Option<LiveDaemonSnapshot>>,
}

/// Captures a live hardware + telemetry snapshot from an unlocked controller.
pub(crate) async fn collect_live_snapshot(
    ctrl: &mut G2010Controller<AnyTransport>,
    daemon_hostname: String,
    cached_ieee1284: &mut Option<String>,
    active_job: Option<JobLockKind>,
) -> Result<LiveDaemonSnapshot, DriverError> {
    let mut report = ctrl.query_status_report().await?;
    let usb_port_status = ctrl.query_port_status().await.ok();

    if cached_ieee1284.is_none() {
        if let Ok(dev_id) = ctrl.query_ieee1284_info().await {
            *cached_ieee1284 = Some(dev_id);
        }
    }

    let mut fw_reported = report.firmware_reported_version;
    if !fw_reported {
        if let Some(ref dev_id) = *cached_ieee1284 {
            if let Some(ver) = extract_ieee1284_tag(dev_id, "VER") {
                report.status.firmware_version = ver;
                fw_reported = true;
            }
        }
    }
    if !fw_reported {
        report.status.firmware_version.clear();
    }

    Ok(LiveDaemonSnapshot {
        daemon_hostname,
        hardware_marker: ctrl.transport().hardware_marker(),
        status: report.status,
        ivec_response: report.ivec_response,
        raw_ivec_status: report.raw_status,
        raw_status_detail: report.raw_status_detail,
        raw_support_code: report.raw_support_code,
        firmware_reported_ink: report.firmware_reported_ink,
        firmware_reported_waste: report.firmware_reported_waste,
        firmware_reported_version: fw_reported,
        ieee1284_id: cached_ieee1284.clone(),
        usb_port_status,
        active_job,
        telemetry_cached_during_job: false,
        queried_at_unix_secs: current_unix_secs(),
    })
}

/// Server listening on a local Unix domain socket and brokering commands to the G2010 controller.
pub struct DaemonServer {
    socket_path: PathBuf,
    listener: UnixListener,
    controller: Arc<Mutex<G2010Controller<AnyTransport>>>,
    arbitrator: Arc<DeviceArbitrator>,
    meta: Arc<SharedDaemonMeta>,
}

impl DaemonServer {
    /// Binds to the specified socket path, cleaning up any stale socket files.
    pub fn bind<P: AsRef<Path>>(
        socket_path: P,
        controller: G2010Controller<AnyTransport>,
    ) -> std::io::Result<Self> {
        let socket_path = socket_path.as_ref().to_path_buf();

        if let Some(parent) = socket_path.parent() {
            if !parent.as_os_str().is_empty() {
                if let Ok(meta) = std::fs::symlink_metadata(parent) {
                    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            format!(
                                "Unsafe socket parent directory (symlink or non-directory): {}",
                                parent.display()
                            ),
                        ));
                    }
                } else {
                    std::fs::create_dir_all(parent)?;
                }
            }
        }

        match std::fs::symlink_metadata(&socket_path) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!(
                            "Refusing to overwrite symlink socket path: {}",
                            socket_path.display()
                        ),
                    ));
                }
                #[cfg(unix)]
                {
                    use std::os::unix::fs::FileTypeExt;
                    if !meta.file_type().is_socket() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::AlreadyExists,
                            format!(
                                "Refusing to unlink non-socket path: {}",
                                socket_path.display()
                            ),
                        ));
                    }
                }
                std::fs::remove_file(&socket_path)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }

        let listener = UnixListener::bind(&socket_path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&socket_path, std::fs::Permissions::from_mode(0o660))?;
        }
        let arbitrator = DeviceArbitrator::new();
        let hw_marker = controller.transport().hardware_marker();
        let meta = Arc::new(SharedDaemonMeta {
            daemon_hostname: local_hostname(),
            hw_marker,
            cached_ieee1284: Mutex::new(None),
            last_snapshot: RwLock::new(None),
        });
        let controller = Arc::new(Mutex::new(controller));

        Ok(Self {
            socket_path,
            listener,
            controller,
            arbitrator,
            meta,
        })
    }

    /// Returns a reference to the server's device arbitrator (for concurrency tests/metrics).
    pub fn arbitrator(&self) -> &Arc<DeviceArbitrator> {
        &self.arbitrator
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
            let meta = Arc::clone(&self.meta);

            tokio::spawn(async move {
                if let Err(err) = Self::handle_client(stream, controller, arbitrator, meta).await {
                    tracing::debug!("Client connection closed: {err}");
                }
            });
        }
    }

    async fn build_busy_snapshot(
        arbitrator: &DeviceArbitrator,
        meta: &SharedDaemonMeta,
    ) -> DaemonResponse {
        let active_job = arbitrator.current_kind();
        let cached = meta.last_snapshot.read().await.clone();
        match cached {
            Some(mut snap) => {
                snap.active_job = active_job;
                snap.telemetry_cached_during_job = true;
                DaemonResponse::LiveSnapshot(Box::new(snap))
            }
            None => DaemonResponse::LiveSnapshot(Box::new(LiveDaemonSnapshot::busy_without_cache(
                meta.daemon_hostname.clone(),
                meta.hw_marker.clone(),
                active_job,
            ))),
        }
    }

    /// Handles a single client connection session.
    async fn handle_client(
        mut stream: UnixStream,
        controller: Arc<Mutex<G2010Controller<AnyTransport>>>,
        arbitrator: Arc<DeviceArbitrator>,
        meta: Arc<SharedDaemonMeta>,
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
                DaemonRequest::GetLiveSnapshot => {
                    if let Some(_guard) = arbitrator.try_acquire(JobLockKind::Telemetry) {
                        if let Ok(mut ctrl) = controller.try_lock() {
                            let mut ieee_guard = meta.cached_ieee1284.lock().await;
                            match collect_live_snapshot(
                                &mut ctrl,
                                meta.daemon_hostname.clone(),
                                &mut ieee_guard,
                                None,
                            )
                            .await
                            {
                                Ok(snap) => {
                                    *meta.last_snapshot.write().await = Some(snap.clone());
                                    DaemonResponse::LiveSnapshot(Box::new(snap))
                                }
                                Err(e) => {
                                    *meta.last_snapshot.write().await = None;
                                    DaemonResponse::Error(e.to_string())
                                }
                            }
                        } else {
                            Self::build_busy_snapshot(&arbitrator, &meta).await
                        }
                    } else {
                        Self::build_busy_snapshot(&arbitrator, &meta).await
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
                    match ctrl.print_raw_stream(&data).await {
                        Ok(()) => DaemonResponse::PrintResult(Ok(())),
                        Err(e) => DaemonResponse::PrintResult(Err(e.to_string())),
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
