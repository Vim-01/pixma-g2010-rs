//! Client interface for interacting with `pixmad` over Unix domain socket
//! with transparent failover to in-process direct hardware/mock execution.

use crate::ipc::{
    local_hostname, recv_response, send_request, ConnectionSource, DaemonRequest, DaemonResponse,
    LiveDaemonSnapshot,
};
use crate::server::collect_live_snapshot;
use pixma_core::errors::DriverError;
use pixma_core::maintenance::{
    MaintenanceAction, MaintenanceCapabilities, MaintenanceRecord, MaintenanceSupervisor,
};
use pixma_core::models::DeviceStatus;
use pixma_core::scan_params::ScanParameters;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

/// Returns the current effective UID via `/proc/self` without libc FFI dependencies.
pub fn current_uid() -> u32 {
    std::fs::metadata("/proc/self")
        .map(|meta| meta.uid())
        .unwrap_or(0)
}

/// Checks read-only whether `dir` exists as a non-symlink directory owned by `uid` with no group/other bits (`0o700`).
pub fn is_verified_private_dir(dir: &Path, uid: u32) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(dir) else {
        return false;
    };
    !meta.file_type().is_symlink()
        && meta.file_type().is_dir()
        && meta.uid() == uid
        && (meta.mode() & 0o077) == 0
}

/// Creates or validates a private `0700` directory owned by `current_uid()`, rejecting symlinks or foreign owners.
pub fn ensure_verified_private_dir(dir: &Path) -> std::io::Result<()> {
    let uid = current_uid();
    match std::fs::symlink_metadata(dir) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(dir) {
                Ok(()) => {}
                Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(create_err) if create_err.kind() == std::io::ErrorKind::NotFound => {
                    std::fs::create_dir_all(dir)?;
                }
                Err(create_err) => return Err(create_err),
            }
        }
        Err(e) => return Err(e),
    }

    let meta = std::fs::symlink_metadata(dir)?;
    if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("Unsafe directory path (symlink or non-directory): {}", dir.display()),
        ));
    }
    if meta.uid() != uid {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("Directory {} is owned by UID {}, expected {uid}", dir.display(), meta.uid()),
        ));
    }
    if (meta.mode() & 0o077) != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        let recheck = std::fs::symlink_metadata(dir)?;
        if recheck.file_type().is_symlink()
            || !recheck.file_type().is_dir()
            || recheck.uid() != uid
            || (recheck.mode() & 0o077) != 0
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Could not enforce 0700 permissions on {}", dir.display()),
            ));
        }
    }
    Ok(())
}

/// Returns ordered local daemon socket candidates without creating directories during discovery.
pub fn default_socket_candidates() -> Vec<PathBuf> {
    default_socket_candidates_with_env(
        std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .as_deref(),
        &std::env::temp_dir(),
        current_uid(),
    )
}

fn default_socket_candidates_with_env(
    xdg_runtime_dir: Option<&Path>,
    temp_root: &Path,
    uid: u32,
) -> Vec<PathBuf> {
    let mut candidates = vec![PathBuf::from("/run/pixma/pixmad.sock")];
    if let Some(xdg) = xdg_runtime_dir {
        if xdg.is_absolute() {
            candidates.push(xdg.join("pixma").join("pixmad.sock"));
        }
    }
    let private_temp_dir = temp_root.join(format!("pixma-daemon-{uid}"));
    if is_verified_private_dir(&private_temp_dir, uid) {
        candidates.push(private_temp_dir.join("pixmad.sock"));
    }
    candidates
}

fn resolve_explicit_socket(socket_path: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = socket_path {
        return Some(p.to_path_buf());
    }
    std::env::var_os("PIXMA_SOCKET")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

fn resolve_default_state_file() -> Option<PathBuf> {
    if let Some(explicit) = std::env::var_os("PIXMA_STATE_FILE").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(explicit));
    }
    if let Some(xdg_state) = std::env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(xdg_state);
        if path.is_absolute() {
            return Some(path.join("pixma-g2010").join("state.json"));
        }
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("pixma-g2010")
                .join("state.json"),
        );
    }
    let fallback_dir = std::env::temp_dir().join(format!("pixma-state-{}", current_uid()));
    ensure_verified_private_dir(&fallback_dir)
        .ok()
        .map(|()| fallback_dir.join("state.json"))
}

fn load_supervisor(state_file: Option<&Path>) -> Result<MaintenanceSupervisor, DriverError> {
    if let Some(p) = state_file {
        MaintenanceSupervisor::load_or_init(p).map_err(DriverError::Io)
    } else if let Some(default_state) = resolve_default_state_file() {
        match MaintenanceSupervisor::load_or_init(&default_state) {
            Ok(sup) => Ok(sup),
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => Err(DriverError::Io(e)),
            Err(_) => Ok(MaintenanceSupervisor::in_memory(
                MaintenanceRecord::default(),
            )),
        }
    } else {
        Ok(MaintenanceSupervisor::in_memory(
            MaintenanceRecord::default(),
        ))
    }
}

enum ClientInner {
    Socket(UnixStream),
    Direct(Arc<Mutex<G2010Controller<AnyTransport>>>),
}

/// Client interface communicating with `pixmad` or falling back to local controller.
pub struct DaemonClient {
    inner: ClientInner,
    source: ConnectionSource,
    direct_hostname: String,
    cached_ieee1284: Option<String>,
}

impl DaemonClient {
    /// Connects to a running `pixmad` daemon via the specified Unix domain socket path.
    pub async fn connect_socket<P: AsRef<Path>>(socket_path: P) -> std::io::Result<Self> {
        let path = socket_path.as_ref();
        let stream = UnixStream::connect(path).await?;
        Ok(Self {
            inner: ClientInner::Socket(stream),
            source: ConnectionSource::DaemonSocket {
                socket_path: path.display().to_string(),
            },
            direct_hostname: local_hostname(),
            cached_ieee1284: None,
        })
    }

    /// Wraps an in-process local controller in direct mode.
    pub fn direct(controller: G2010Controller<AnyTransport>) -> Self {
        let source = if controller.transport().is_mock() {
            ConnectionSource::DirectMock
        } else {
            ConnectionSource::DirectUsb
        };
        Self {
            inner: ClientInner::Direct(Arc::new(Mutex::new(controller))),
            source,
            direct_hostname: local_hostname(),
            cached_ieee1284: None,
        }
    }

    /// Connects strictly to real hardware (`pixmad` Unix socket or direct physical USB).
    ///
    /// Unlike [`Self::connect_or_fallback`], this never falls back to `MockG2010Transport`
    /// so UI/monitoring callers can report genuine hardware disconnection states.
    pub async fn connect_live(
        socket_path: Option<&Path>,
        state_file: Option<&Path>,
    ) -> Result<Self, DriverError> {
        if let Some(explicit) = resolve_explicit_socket(socket_path) {
            return Self::connect_socket(&explicit).await.map_err(|e| {
                DriverError::Transport(format!(
                    "Daemon socket {} is unavailable: {e}",
                    explicit.display()
                ))
            });
        }

        for candidate in default_socket_candidates() {
            if candidate.exists() {
                if let Ok(client) = Self::connect_socket(&candidate).await {
                    return Ok(client);
                }
            }
        }

        let devices = discover_canon_devices()?;
        if devices.is_empty() {
            return Err(DriverError::Transport(
                "No Canon PIXMA printer found on local USB bus and pixmad socket is unavailable"
                    .to_string(),
            ));
        }

        let nusb = NusbTransport::open_first()?;
        let supervisor = load_supervisor(state_file)?;
        let controller = G2010Controller::new(AnyTransport::Usb(nusb), supervisor);
        Ok(Self::direct(controller))
    }

    /// Connects synchronously to an in-process simulated `MockG2010Transport` without mutating process env vars.
    pub fn connect_mock(state_file: Option<&Path>) -> Result<Self, DriverError> {
        let mock = MockG2010Transport::new();
        let supervisor = match state_file {
            Some(p) => MaintenanceSupervisor::load_or_init(p).map_err(DriverError::Io)?,
            None => MaintenanceSupervisor::in_memory(MaintenanceRecord::default()),
        };
        let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
        Ok(Self::direct(ctrl))
    }

    /// Connects to `pixmad` or automatically falls back to direct controller execution.
    ///
    /// Respects `PIXMA_MOCK=1` and `PIXMA_SOCKET` environment variables.
    pub async fn connect_or_fallback(
        socket_path: Option<&Path>,
        state_file: Option<&Path>,
    ) -> Result<Self, DriverError> {
        // 1. Force mock mode via environment variable
        if std::env::var("PIXMA_MOCK").as_deref() == Ok("1") {
            tracing::info!("PIXMA_MOCK=1 set: using in-memory virtual G2010 simulator");
            return Self::connect_mock(state_file);
        }

        // 2. Try socket connection (explicit path / PIXMA_SOCKET, else default daemon sockets)
        if let Some(explicit) = resolve_explicit_socket(socket_path) {
            if explicit.exists() {
                if let Ok(client) = Self::connect_socket(&explicit).await {
                    return Ok(client);
                }
            }
        } else {
            for candidate in default_socket_candidates() {
                if candidate.exists() {
                    if let Ok(client) = Self::connect_socket(&candidate).await {
                        return Ok(client);
                    }
                }
            }
        }

        // 3. Fallback to direct local transport (real USB or Mock)
        tracing::info!("pixmad socket not found; falling back to direct transport");
        let transport = if let Ok(devices) = discover_canon_devices() {
            if !devices.is_empty() {
                match NusbTransport::open_first() {
                    Ok(nusb) => AnyTransport::Usb(nusb),
                    Err(err) => {
                        tracing::warn!("Failed to open physical USB device: {err}; using mock");
                        AnyTransport::Mock(Box::new(MockG2010Transport::new()))
                    }
                }
            } else {
                AnyTransport::Mock(Box::new(MockG2010Transport::new()))
            }
        } else {
            AnyTransport::Mock(Box::new(MockG2010Transport::new()))
        };

        let supervisor = load_supervisor(state_file)?;
        let controller = G2010Controller::new(transport, supervisor);
        Ok(Self::direct(controller))
    }

    /// Returns how this client is connected (`DaemonSocket`, `DirectUsb`, or `DirectMock`).
    pub fn source(&self) -> &ConnectionSource {
        &self.source
    }

    /// Returns true if the client is connected directly to simulated virtual hardware.
    pub async fn is_mock(&self) -> bool {
        match &self.inner {
            ClientInner::Direct(ctrl) => {
                let guard = ctrl.lock().await;
                guard.transport().is_mock()
            }
            ClientInner::Socket(_) => false,
        }
    }

    /// Queries full non-blocking live snapshot (USB marker, host identity, IVEC raw status,
    /// IEEE-1284 ID, USB port status byte, and active job arbitration state).
    pub async fn get_live_snapshot(&mut self) -> Result<LiveDaemonSnapshot, DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                send_request(stream, &DaemonRequest::GetLiveSnapshot)
                    .await
                    .map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::LiveSnapshot(snap) => Ok(*snap),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                collect_live_snapshot(
                    &mut guard,
                    self.direct_hostname.clone(),
                    &mut self.cached_ieee1284,
                    None,
                )
                .await
            }
        }
    }

    /// Pings the daemon or local controller.
    pub async fn ping(&mut self) -> Result<(), DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                send_request(stream, &DaemonRequest::Ping)
                    .await
                    .map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::Pong => Ok(()),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(_) => Ok(()),
        }
    }

    /// Queries real-time hardware status and telemetry.
    pub async fn get_status(&mut self) -> Result<DeviceStatus, DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                send_request(stream, &DaemonRequest::GetStatus)
                    .await
                    .map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::Status(status) => Ok(status),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                guard.query_status().await
            }
        }
    }

    /// Queries live `GetCapability(servicetype="maintenance")` from firmware,
    /// mapping legacy daemon disconnect/EOF errors clearly when connected to `pixmad < 0.3.1`.
    pub async fn get_maintenance_capabilities(
        &mut self,
    ) -> Result<MaintenanceCapabilities, DriverError> {
        fn map_compat_io_err(e: std::io::Error) -> DriverError {
            match e.kind() {
                std::io::ErrorKind::UnexpectedEof
                | std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset => DriverError::Protocol(format!(
                    "Daemon disconnected on GetMaintenanceCapabilities (running pixmad may be older than v0.3.1 and need restart/upgrade): {e}"
                )),
                _ => DriverError::Io(e),
            }
        }

        match &mut self.inner {
            ClientInner::Socket(stream) => {
                send_request(stream, &DaemonRequest::GetMaintenanceCapabilities)
                    .await
                    .map_err(map_compat_io_err)?;
                match recv_response(stream).await.map_err(map_compat_io_err)? {
                    DaemonResponse::MaintenanceCapabilities(Ok(caps)) => Ok(caps),
                    DaemonResponse::MaintenanceCapabilities(Err(e)) => {
                        Err(DriverError::Protocol(e))
                    }
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                guard.query_maintenance_capabilities().await
            }
        }
    }

    /// Triggers an authorized maintenance routine.
    pub async fn execute_maintenance(
        &mut self,
        action: MaintenanceAction,
        confirmation_token: Option<&str>,
        override_cooldown: bool,
    ) -> Result<DeviceStatus, DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                let req = DaemonRequest::ExecuteMaintenance {
                    action,
                    confirmation_token: confirmation_token.map(|s| s.to_string()),
                    override_cooldown,
                };
                send_request(stream, &req).await.map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::MaintenanceResult(Ok(status)) => Ok(status),
                    DaemonResponse::MaintenanceResult(Err(e)) => Err(DriverError::Protocol(e)),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                guard
                    .execute_maintenance(action, confirmation_token, override_cooldown)
                    .await
            }
        }
    }

    /// Streams raw bytes directly to the printer interface bulk OUT pipe.
    pub async fn print_raw_stream(&mut self, data: &[u8]) -> Result<(), DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                let req = DaemonRequest::PrintRawStream {
                    data: data.to_vec(),
                };
                send_request(stream, &req).await.map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::PrintResult(Ok(())) => Ok(()),
                    DaemonResponse::PrintResult(Err(e)) => Err(DriverError::Protocol(e)),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                guard.print_raw_stream(data).await
            }
        }
    }

    /// Acquires raw image scan data conforming to MP2 protocol.
    pub async fn acquire_scan(&mut self, params: &ScanParameters) -> Result<Vec<u8>, DriverError> {
        match &mut self.inner {
            ClientInner::Socket(stream) => {
                let req = DaemonRequest::AcquireScan {
                    params: params.clone(),
                };
                send_request(stream, &req).await.map_err(DriverError::Io)?;
                match recv_response(stream).await.map_err(DriverError::Io)? {
                    DaemonResponse::ScanData(Ok(bytes)) => Ok(bytes),
                    DaemonResponse::ScanData(Err(e)) => Err(DriverError::Protocol(e)),
                    DaemonResponse::Error(e) => Err(DriverError::Protocol(e)),
                    other => Err(DriverError::Protocol(format!(
                        "Unexpected response: {other:?}"
                    ))),
                }
            }
            ClientInner::Direct(ctrl) => {
                let mut guard = ctrl.lock().await;
                guard.acquire_raw_scan(params).await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn temp_test_dir() -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "pixma-client-paths-{}-{seq}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn default_socket_candidates_are_read_only_and_verify_0700_private_dir() {
        let root = temp_test_dir();
        let uid = current_uid();
        let xdg_dir = root.join("xdg");

        // When private temp dir does not exist, probe MUST NOT create it
        let cands = default_socket_candidates_with_env(Some(&xdg_dir), &root, uid);
        let expected_private = root.join(format!("pixma-daemon-{uid}"));
        assert!(!expected_private.exists());
        assert_eq!(
            cands,
            vec![
                PathBuf::from("/run/pixma/pixmad.sock"),
                xdg_dir.join("pixma").join("pixmad.sock"),
            ]
        );

        // With 0755 permissions, private dir MUST be skipped
        std::fs::create_dir_all(&expected_private).unwrap();
        std::fs::set_permissions(&expected_private, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cands_loose = default_socket_candidates_with_env(None, &root, uid);
        assert_eq!(cands_loose, vec![PathBuf::from("/run/pixma/pixmad.sock")]);

        // With 0700 permissions owned by uid, private dir IS included
        std::fs::set_permissions(&expected_private, std::fs::Permissions::from_mode(0o700)).unwrap();
        let cands_ok = default_socket_candidates_with_env(None, &root, uid);
        assert_eq!(
            cands_ok,
            vec![
                PathBuf::from("/run/pixma/pixmad.sock"),
                expected_private.join("pixmad.sock"),
            ]
        );

        // Symlink MUST be rejected
        std::fs::remove_dir(&expected_private).unwrap();
        let target = root.join("symlink-target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::os::unix::fs::symlink(&target, &expected_private).unwrap();
        assert_eq!(
            default_socket_candidates_with_env(None, &root, uid),
            vec![PathBuf::from("/run/pixma/pixmad.sock")]
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn ensure_verified_private_dir_creates_0700_and_rejects_symlink() {
        let root = temp_test_dir();
        let dir = root.join("priv");
        ensure_verified_private_dir(&dir).unwrap();
        assert_eq!(
            std::fs::metadata(&dir).unwrap().permissions().mode() & 0o777,
            0o700
        );

        let sym = root.join("sym");
        std::os::unix::fs::symlink(&dir, &sym).unwrap();
        assert!(ensure_verified_private_dir(&sym).is_err());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn load_supervisor_rejects_corrupted_state_json() {
        let root = temp_test_dir();
        let corrupt_path = root.join("state.json");
        std::fs::write(&corrupt_path, b"{corrupt json").unwrap();

        let err = load_supervisor(Some(&corrupt_path))
            .expect_err("Corrupted explicit state file must fail with DriverError::Io(InvalidData)");
        assert!(matches!(
            err,
            DriverError::Io(ref io_err) if io_err.kind() == std::io::ErrorKind::InvalidData
        ));

        let _ = std::fs::remove_dir_all(&root);
    }
}
