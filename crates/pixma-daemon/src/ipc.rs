//! Inter-process communication (IPC) protocol, typed messages, and USB resource arbitrator.
//!
//! Exposes request/response framing over Unix domain sockets with length-prefixed
//! payloads (supporting JSON and CBOR), and arbitration tokens preventing concurrent
//! USB interface collisions.

use pixma_core::maintenance::MaintenanceAction;
use pixma_core::models::{DeviceStatus, OperationalState, UsbHardwareMarker};
use pixma_core::scan_params::ScanParameters;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{Mutex, MutexGuard};

/// Returns the current host's hostname without extra FFI dependencies.
pub fn local_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .or_else(|_| std::fs::read_to_string("/etc/hostname"))
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|s| !s.trim().is_empty()))
        .unwrap_or_else(|| "localhost".to_string())
}

/// Identifies how a client is attached to the device controller.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionSource {
    /// Connected to a `pixmad` Unix domain socket (local or bridged over SSH).
    DaemonSocket { socket_path: String },
    /// Direct in-process physical USB `nusb` transport.
    DirectUsb,
    /// Simulated virtual emulator (`--mock` / `PIXMA_MOCK=1`).
    DirectMock,
}

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

impl JobLockKind {
    fn as_code(self) -> u8 {
        match self {
            Self::Printing => 1,
            Self::Scanning => 2,
            Self::Maintenance => 3,
            Self::Telemetry => 4,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::Printing),
            2 => Some(Self::Scanning),
            3 => Some(Self::Maintenance),
            4 => Some(Self::Telemetry),
            _ => None,
        }
    }
}

/// Real-time hardware + daemon telemetry snapshot returned by `DaemonRequest::GetLiveSnapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveDaemonSnapshot {
    /// Hostname of the machine owning the USB bus (e.g. `usb-server` vs `workstation`).
    pub daemon_hostname: String,
    /// Live USB hardware bus/device/VID:PID/endpoints locator from the daemon/transport.
    pub hardware_marker: UsbHardwareMarker,
    /// Structured device status parsed from IVEC `GetStatusResponse`.
    pub status: DeviceStatus,
    /// `<ivec:response>` string reported by firmware (e.g. `"OK"`), if present.
    pub ivec_response: Option<String>,
    /// Raw `<ivec:status>` string reported by firmware (e.g. `"idle"`, `"processing"`).
    pub raw_ivec_status: Option<String>,
    /// Raw `<vcn:statusdetail>` / `<ivec:status_detail>` string, if present.
    pub raw_status_detail: Option<String>,
    /// Raw `<ivec:current_support_code>` / `<vcn:supportcode>` string, if present.
    pub raw_support_code: Option<String>,
    /// Whether the IVEC XML actually included ink percentage tags.
    pub firmware_reported_ink: bool,
    /// Whether the IVEC XML actually included waste absorber / D-counter percentage tags.
    pub firmware_reported_waste: bool,
    /// Whether firmware version came from IVEC XML or IEEE-1284 `VER:`.
    pub firmware_reported_version: bool,
    /// IEEE-1284 `GET_DEVICE_ID` string read from hardware, if available.
    pub ieee1284_id: Option<String>,
    /// USB Printer Class `GET_PORT_STATUS` byte (bit 5=Paper Empty, bit 4=Select, bit 3=Not Error).
    pub usb_port_status: Option<u8>,
    /// Currently active exclusive operation in `DeviceArbitrator`, if any.
    pub active_job: Option<JobLockKind>,
    /// True when returned without blocking because an exclusive job held the USB mutex.
    pub telemetry_cached_during_job: bool,
    /// Unix epoch timestamp (seconds) when the snapshot was captured.
    pub queried_at_unix_secs: u64,
}

impl LiveDaemonSnapshot {
    /// Builds an active-job snapshot when no prior cached telemetry exists yet.
    pub fn busy_without_cache(
        daemon_hostname: String,
        hardware_marker: UsbHardwareMarker,
        active_job: Option<JobLockKind>,
    ) -> Self {
        let state = match active_job {
            Some(JobLockKind::Printing) => OperationalState::Printing,
            Some(JobLockKind::Scanning) => OperationalState::Scanning,
            Some(JobLockKind::Maintenance) => OperationalState::Cleaning,
            Some(JobLockKind::Telemetry) | None => OperationalState::Waiting,
        };
        let status = DeviceStatus {
            serial_number: hardware_marker.serial_number.clone(),
            firmware_version: String::new(),
            state,
            status_detail: format!("{state:?}").to_lowercase(),
            ..Default::default()
        };

        Self {
            daemon_hostname,
            hardware_marker,
            status,
            ivec_response: None,
            raw_ivec_status: None,
            raw_status_detail: None,
            raw_support_code: None,
            firmware_reported_ink: false,
            firmware_reported_waste: false,
            firmware_reported_version: false,
            ieee1284_id: None,
            usb_port_status: None,
            active_job,
            telemetry_cached_during_job: true,
            queried_at_unix_secs: current_unix_secs(),
        }
    }
}

/// Returns current UNIX epoch time in seconds.
pub fn current_unix_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
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
    active_code: Arc<AtomicU8>,
}

/// RAII lock guard representing an active, authorized hardware session.
pub struct JobLockGuard<'a> {
    kind: JobLockKind,
    guard: MutexGuard<'a, ArbitratorState>,
    active_code: Arc<AtomicU8>,
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
        self.active_code.store(0, Ordering::SeqCst);
    }
}

impl DeviceArbitrator {
    /// Creates a new device arbitrator.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            mutex: Mutex::new(ArbitratorState::default()),
            active_code: Arc::new(AtomicU8::new(0)),
        })
    }

    /// Wait-free inspection of the currently active hardware lock kind, if any.
    pub fn current_kind(&self) -> Option<JobLockKind> {
        JobLockKind::from_code(self.active_code.load(Ordering::SeqCst))
    }

    /// Asynchronously acquires the device lock for the specified kind of operation.
    pub async fn acquire(&self, kind: JobLockKind) -> JobLockGuard<'_> {
        let guard = self.mutex.lock().await;
        self.wrap_guard(kind, guard)
    }

    /// Attempts to acquire the device lock without waiting behind an ongoing job.
    pub fn try_acquire(&self, kind: JobLockKind) -> Option<JobLockGuard<'_>> {
        let guard = self.mutex.try_lock().ok()?;
        Some(self.wrap_guard(kind, guard))
    }

    fn wrap_guard<'a>(
        &self,
        kind: JobLockKind,
        mut guard: MutexGuard<'a, ArbitratorState>,
    ) -> JobLockGuard<'a> {
        guard.active_kind = Some(kind);
        self.active_code.store(kind.as_code(), Ordering::SeqCst);
        JobLockGuard {
            kind,
            guard,
            active_code: Arc::clone(&self.active_code),
        }
    }
}

/// Maximum allowed serialized JSON IPC frame payload (16 MiB).
pub const MAX_IPC_JSON_BYTES: usize = 16 * 1024 * 1024;

/// Maximum allowed serialized CBOR IPC frame payload (512 MiB, covers high-DPI raster/scan streams).
pub const MAX_IPC_CBOR_BYTES: usize = 512 * 1024 * 1024;

mod raw_bytes_serde {
    use serde::de::{SeqAccess, Visitor};
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S>(bytes: &[u8], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(bytes)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct BytesVisitor;

        impl<'de> Visitor<'de> for BytesVisitor {
            type Value = Vec<u8>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a byte buffer or sequence of u8")
            }

            fn visit_bytes<E>(self, v: &[u8]) -> Result<Self::Value, E> {
                Ok(v.to_vec())
            }

            fn visit_byte_buf<E>(self, v: Vec<u8>) -> Result<Self::Value, E> {
                Ok(v)
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let hint = seq.size_hint().unwrap_or(0).min(1024 * 1024);
                let mut out = Vec::with_capacity(hint);
                while let Some(byte) = seq.next_element()? {
                    out.push(byte);
                }
                Ok(out)
            }
        }

        deserializer.deserialize_any(BytesVisitor)
    }
}

mod raw_bytes_result_serde {
    use super::raw_bytes_serde;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    #[derive(Serialize)]
    #[serde(rename = "Result")]
    enum ResultRef<'a> {
        Ok(#[serde(serialize_with = "raw_bytes_serde::serialize")] &'a [u8]),
        Err(&'a str),
    }

    #[derive(Deserialize)]
    #[serde(rename = "Result")]
    enum ResultOwned {
        Ok(#[serde(with = "raw_bytes_serde")] Vec<u8>),
        Err(String),
    }

    pub fn serialize<S>(value: &Result<Vec<u8>, String>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match value {
            Ok(bytes) => ResultRef::Ok(bytes.as_slice()).serialize(serializer),
            Err(err) => ResultRef::Err(err.as_str()).serialize(serializer),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Result<Vec<u8>, String>, D::Error>
    where
        D: Deserializer<'de>,
    {
        match ResultOwned::deserialize(deserializer)? {
            ResultOwned::Ok(bytes) => Ok(Ok(bytes)),
            ResultOwned::Err(err) => Ok(Err(err)),
        }
    }
}

/// Requests accepted by the `pixmad` daemon.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DaemonRequest {
    /// Connectivity health check.
    Ping,
    /// Queries real-time hardware status and ink levels.
    GetStatus,
    /// Queries full non-blocking hardware + USB marker + arbitrator state snapshot.
    GetLiveSnapshot,
    /// Triggers an authorized maintenance action.
    ExecuteMaintenance {
        action: MaintenanceAction,
        confirmation_token: Option<String>,
        override_cooldown: bool,
    },
    /// Streams raw bytes directly to the printer interface bulk OUT pipe.
    PrintRawStream {
        #[serde(with = "raw_bytes_serde")]
        data: Vec<u8>,
    },
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
    /// Non-blocking full hardware/daemon telemetry snapshot.
    LiveSnapshot(Box<LiveDaemonSnapshot>),
    /// Result of an executed maintenance action.
    MaintenanceResult(Result<DeviceStatus, String>),
    /// Result of a raw print stream operation.
    PrintResult(Result<(), String>),
    /// Acquired raw scan data bytes.
    ScanData(#[serde(with = "raw_bytes_result_serde")] Result<Vec<u8>, String>),
    /// Unspecified error or rejected request.
    Error(String),
}

// ---------------------------------------------------------------------------
// Framing Primitives (4-byte big-endian length-prefixed payloads)
// ---------------------------------------------------------------------------

async fn write_bounded_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    payload: &[u8],
    max_len: usize,
) -> std::io::Result<()> {
    if payload.len() > max_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "IPC frame payload length ({} bytes) exceeds maximum allowed ({max_len} bytes)",
                payload.len()
            ),
        ));
    }
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "IPC frame length exceeds 32-bit wire prefix",
        )
    })?;
    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

async fn read_bounded_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
    max_len: usize,
) -> std::io::Result<Vec<u8>> {
    let mut len_buf = [0u8; 4];
    reader.read_exact(&mut len_buf).await?;
    let len = u32::from_be_bytes(len_buf) as usize;
    if len > max_len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Incoming IPC frame length ({len} bytes) exceeds maximum allowed ({max_len} bytes)"),
        ));
    }
    let mut payload = vec![0u8; len];
    reader.read_exact(&mut payload).await?;
    Ok(payload)
}

/// Writes a length-prefixed JSON frame into `writer`.
pub async fn write_frame_json<W: AsyncWrite + Unpin, T: Serialize>(
    writer: &mut W,
    value: &T,
) -> std::io::Result<()> {
    let payload = serde_json::to_vec(value)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    write_bounded_frame(writer, &payload, MAX_IPC_JSON_BYTES).await
}

/// Reads a length-prefixed JSON frame from `reader`.
pub async fn read_frame_json<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> std::io::Result<T> {
    let payload = read_bounded_frame(reader, MAX_IPC_JSON_BYTES).await?;
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
    write_bounded_frame(writer, &payload, MAX_IPC_CBOR_BYTES).await
}

/// Reads a length-prefixed CBOR frame from `reader`.
pub async fn read_frame_cbor<R: AsyncRead + Unpin, T: DeserializeOwned>(
    reader: &mut R,
) -> std::io::Result<T> {
    let payload = read_bounded_frame(reader, MAX_IPC_CBOR_BYTES).await?;
    ciborium::from_reader(&payload[..])
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))
}

/// Helper sending a `DaemonRequest` over an async writer (CBOR with raw byte strings).
pub async fn send_request<W: AsyncWrite + Unpin>(
    writer: &mut W,
    req: &DaemonRequest,
) -> std::io::Result<()> {
    write_frame_cbor(writer, req).await
}

/// Helper receiving a `DaemonRequest` from an async reader (CBOR with raw byte strings).
pub async fn recv_request<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<DaemonRequest> {
    read_frame_cbor(reader).await
}

/// Helper sending a `DaemonResponse` over an async writer (CBOR with raw byte strings).
pub async fn send_response<W: AsyncWrite + Unpin>(
    writer: &mut W,
    resp: &DaemonResponse,
) -> std::io::Result<()> {
    write_frame_cbor(writer, resp).await
}

/// Helper receiving a `DaemonResponse` from an async reader (CBOR with raw byte strings).
pub async fn recv_response<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<DaemonResponse> {
    read_frame_cbor(reader).await
}
