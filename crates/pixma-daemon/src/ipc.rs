//! Inter-process communication (IPC) protocol, typed messages, and USB resource arbitrator.
//!
//! Exposes request/response framing over Unix domain sockets with length-prefixed
//! payloads (supporting JSON and CBOR), and arbitration tokens preventing concurrent
//! USB interface collisions.

use pixma_core::maintenance::MaintenanceAction;
use pixma_core::models::DeviceStatus;
use pixma_core::scan_params::ScanParameters;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, MutexGuard};

/// Resource arbitration token category protecting the shared hardware chassis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum JobLockKind {
    /// Exclusive printhead raster streaming session.
    Printing,
    /// Exclusive optical scanner carriage acquisition session.
    Scanning,
    /// Exclusive maintenance routine (cleaning, test print, alignment).
    Maintenance,
    /// Low-priority background telemetry status query.
    Telemetry,
}

/// Active arbitration state managed under an asynchronous lock.
#[derive(Debug, Default)]
struct ArbitratorState {
    active_kind: Option<JobLockKind>,
}

/// Token-based resource arbitrator ensuring that printing, scanning, maintenance,
/// and telemetry polling never collide on the USB bus.
#[derive(Debug, Default)]
pub struct DeviceArbitrator {
    mutex: Mutex<ArbitratorState>,
}

/// RAII lock guard representing an active, authorized hardware session.
pub struct JobLockGuard<'a> {
    kind: JobLockKind,
    guard: MutexGuard<'a, ArbitratorState>,
}

impl<'a> JobLockGuard<'a> {
    /// Returns the active lock kind held by this guard.
    pub fn kind(&self) -> JobLockKind {
        self.kind
    }
}

impl<'a> Drop for JobLockGuard<'a> {
    fn drop(&mut self) {
        self.guard.active_kind = None;
    }
}

impl DeviceArbitrator {
    /// Creates a new device arbitrator.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: Mutex::new(ArbitratorState::default()),
        })
    }

    /// Asynchronously acquires the device lock for the specified kind of operation.
    pub async fn acquire(&self, kind: JobLockKind) -> JobLockGuard<'_> {
        let mut guard = self.mutex.lock().await;
        guard.active_kind = Some(kind);
        JobLockGuard { kind, guard }
    }
}

/// Requests accepted by the `pixmad` daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Connectivity health check.
    Ping,
    /// Queries real-time hardware status and ink levels.
    GetStatus,
    /// Triggers an authorized maintenance action.
    ExecuteMaintenance {
        action: MaintenanceAction,
        confirmation_token: Option<String>,
        override_cooldown: bool,
    },
    /// Streams raw bytes directly to the printer interface bulk OUT pipe.
    PrintRawStream { data: Vec<u8> },
    /// Executes a scan according to the requested optical and geometry parameters.
    AcquireScan { params: ScanParameters },
}

/// Responses emitted by the `pixmad` daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonResponse {
    /// Health check reply.
    Pong,
    /// Device telemetry snapshot.
    Status(DeviceStatus),
    /// Result of an executed maintenance action.
    MaintenanceResult(Result<DeviceStatus, String>),
    /// Result of a raw print stream operation.
    PrintResult(Result<(), String>),
    /// Acquired raw scan data bytes.
    ScanData(Result<Vec<u8>, String>),
    /// Unspecified error or rejected request.
    Error(String),
}

// ---------------------------------------------------------------------------
// Framing Primitives (4-byte big-endian length-prefixed payloads)
// ---------------------------------------------------------------------------

/// Writes a length-prefixed JSON frame into `writer`.
pub async fn write_frame_json<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads a length-prefixed JSON frame from `reader`.
pub async fn read_frame_json<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;

    serde_json::from_slice(&payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Writes a length-prefixed CBOR frame into `writer`.
pub async fn write_frame_cbor<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> std::io::Result<()> {
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?;
    let len = payload.len() as u32;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Reads a length-prefixed CBOR frame from `reader`.
pub async fn read_frame_cbor<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> std::io::Result<T> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;

    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;

    ciborium::from_reader(&payload[..])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Helper sending a `DaemonRequest` over an async writer.
pub async fn send_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    req: &DaemonRequest,
) -> std::io::Result<()> {
    write_frame_json(writer, req).await
}

/// Helper receiving a `DaemonRequest` from an async reader.
pub async fn recv_request<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<DaemonRequest> {
    read_frame_json(reader).await
}

/// Helper sending a `DaemonResponse` over an async writer.
pub async fn send_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    resp: &DaemonResponse,
) -> std::io::Result<()> {
    write_frame_json(writer, resp).await
}

/// Helper receiving a `DaemonResponse` from an async reader.
pub async fn recv_response<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<DaemonResponse> {
    read_frame_json(reader).await
}
