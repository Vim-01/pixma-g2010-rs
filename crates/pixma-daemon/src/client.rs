//! Client interface for interacting with `pixmad` over Unix domain socket
//! with transparent failover to in-process direct hardware/mock execution.

use crate::ipc::{recv_response, send_request, DaemonRequest, DaemonResponse};
use pixma_core::errors::DriverError;
use pixma_core::maintenance::{MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor};
use pixma_core::models::DeviceStatus;
use pixma_core::scan_params::ScanParameters;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};
use std::path::Path;
use std::sync::Arc;
use tokio::net::UnixStream;
use tokio::sync::Mutex;

enum ClientInner {
    Socket(UnixStream),
    Direct(Arc<Mutex<G2010Controller<AnyTransport>>>),
}

/// Client interface communicating with `pixmad` or falling back to local controller.
pub struct DaemonClient {
    inner: ClientInner,
}

impl DaemonClient {
    /// Connects to a running `pixmad` daemon via the specified Unix domain socket path.
    pub async fn connect_socket<P: AsRef<Path>>(socket_path: P) -> std::io::Result<Self> {
        let stream = UnixStream::connect(socket_path).await?;
        Ok(Self {
            inner: ClientInner::Socket(stream),
        })
    }

    /// Wraps an in-process local controller in direct mode.
    pub fn direct(controller: G2010Controller<AnyTransport>) -> Self {
        Self {
            inner: ClientInner::Direct(Arc::new(Mutex::new(controller))),
        }
    }

    /// Connects to `pixmad` or automatically falls back to direct controller execution.
    ///
    /// Respects `PIXMA_MOCK=1` environment variable to force simulated hardware.
    pub async fn connect_or_fallback(
        socket_path: Option<&Path>,
        state_file: Option<&Path>,
    ) -> Result<Self, DriverError> {
        // 1. Force mock mode via environment variable
        if std::env::var("PIXMA_MOCK").as_deref() == Ok("1") {
            tracing::info!("PIXMA_MOCK=1 set: using in-memory virtual G2010 simulator");
            let mock = MockG2010Transport::new();
            let supervisor = match state_file {
                Some(p) => MaintenanceSupervisor::load_or_init(p).map_err(DriverError::Io)?,
                None => MaintenanceSupervisor::in_memory(MaintenanceRecord::default()),
            };
            let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
            return Ok(Self::direct(ctrl));
        }

        // 2. Try socket connection
        let candidates: Vec<&Path> = if let Some(p) = socket_path {
            vec![p]
        } else {
            vec![
                Path::new("/run/pixma/pixmad.sock"),
                Path::new("/tmp/pixmad.sock"),
            ]
        };

        for path in candidates {
            if path.exists() {
                if let Ok(client) = Self::connect_socket(path).await {
                    return Ok(client);
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

        let supervisor = if let Some(p) = state_file {
            MaintenanceSupervisor::load_or_init(p).map_err(DriverError::Io)?
        } else {
            let default_state = Path::new("/tmp/pixma-state.json");
            MaintenanceSupervisor::load_or_init(default_state)
                .unwrap_or_else(|_| MaintenanceSupervisor::in_memory(MaintenanceRecord::default()))
        };

        let controller = G2010Controller::new(transport, supervisor);
        Ok(Self::direct(controller))
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
