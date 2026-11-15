//! Native asynchronous USB transport implemented via `nusb`.
//!
//! Provides direct interaction with Linux `usbfs`, kernel driver detachment,
//! cooperative file-based session locking (`flock`), and IEEE-1284 / MP2 bulk pipes.

use crate::traits::UsbTransport;
use nix::fcntl::Flock;
use nusb::transfer::{
    ControlIn, ControlOut, ControlType, Direction, EndpointType, Recipient, RequestBuffer,
};
use pixma_core::errors::DriverError;
use pixma_core::models::{
    PrinterModel, UsbHardwareMarker, CANON_USB_VID, G1010_USB_PID, G2000_USB_PID, G2010_USB_PID,
    G2020_USB_PID, G3000_USB_PID, G3010_USB_PID, G4000_USB_PID, G4010_USB_PID,
};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// All supported Canon G-series USB product identifiers.
pub const SUPPORTED_PIDS: &[u16] = &[
    G1010_USB_PID,
    G2000_USB_PID,
    G2010_USB_PID,
    G2020_USB_PID,
    G3000_USB_PID,
    G3010_USB_PID,
    G4000_USB_PID,
    G4010_USB_PID,
];

/// Cooperative file lock preventing multiple standalone processes from colliding
/// on the physical USB interfaces.
pub struct FileLockGuard {
    lock: Option<Flock<std::fs::File>>,
    path: PathBuf,
}

impl FileLockGuard {
    fn current_uid() -> u32 {
        std::fs::metadata("/proc/self")
            .map(|meta| meta.uid())
            .unwrap_or(0)
    }

    fn ensure_private_lock_dir(dir: &Path, uid: u32) -> Result<(), DriverError> {
        match std::fs::symlink_metadata(dir) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match std::fs::create_dir(dir) {
                    Ok(()) => {}
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::AlreadyExists => {}
                    Err(create_err) if create_err.kind() == std::io::ErrorKind::NotFound => {
                        std::fs::create_dir_all(dir).map_err(DriverError::Io)?;
                    }
                    Err(create_err) => return Err(DriverError::Io(create_err)),
                }
            }
            Err(e) => return Err(DriverError::Io(e)),
        }

        let meta = std::fs::symlink_metadata(dir).map_err(DriverError::Io)?;
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            return Err(DriverError::Transport(format!(
                "Unsafe fallback lock path (symlink or non-directory): {}",
                dir.display()
            )));
        }
        if meta.uid() != uid {
            return Err(DriverError::Transport(format!(
                "Fallback lock directory {} is owned by UID {}, expected UID {uid}",
                dir.display(),
                meta.uid()
            )));
        }
        if (meta.mode() & 0o077) != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(DriverError::Io)?;
            let recheck = std::fs::symlink_metadata(dir).map_err(DriverError::Io)?;
            if recheck.file_type().is_symlink()
                || !recheck.file_type().is_dir()
                || recheck.uid() != uid
                || (recheck.mode() & 0o077) != 0
            {
                return Err(DriverError::Transport(format!(
                    "Could not enforce 0700 permissions on {}",
                    dir.display()
                )));
            }
        }
        Ok(())
    }

    fn open_primary_lock_file(primary_path: &Path) -> Result<std::fs::File, std::io::Error> {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o664)
            .open(primary_path)
        {
            Ok(file) => {
                let _ = file.set_permissions(std::fs::Permissions::from_mode(0o664));
                Ok(file)
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                match std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(primary_path)
                {
                    Ok(file) => Ok(file),
                    Err(rw_err) if rw_err.kind() == std::io::ErrorKind::PermissionDenied => {
                        // Fall back to read-only without O_CREAT so unprivileged desktop
                        // uaccess users lock the exact same /run/lock inode even under
                        // fs.protected_regular >= 1.
                        std::fs::OpenOptions::new().read(true).open(primary_path)
                    }
                    Err(rw_err) => Err(rw_err),
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                std::fs::OpenOptions::new().read(true).open(primary_path)
            }
            Err(e) => Err(e),
        }
    }

    fn acquire_with_paths(primary_path: &Path, fallback_dir: &Path) -> Result<Self, DriverError> {
        let (file, path) = match Self::open_primary_lock_file(primary_path) {
            Ok(f) => (f, primary_path.to_path_buf()),
            Err(_) => {
                let uid = Self::current_uid();
                Self::ensure_private_lock_dir(fallback_dir, uid)?;
                let fallback_path = fallback_dir.join("pixma-g2010.lock");
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .mode(0o600)
                    .open(&fallback_path)
                    .map_err(DriverError::Io)?;
                let f_meta = std::fs::symlink_metadata(&fallback_path).map_err(DriverError::Io)?;
                if f_meta.file_type().is_symlink() || !f_meta.file_type().is_file() || f_meta.uid() != uid {
                    return Err(DriverError::Transport(format!(
                        "Unsafe fallback lock file: {}",
                        fallback_path.display()
                    )));
                }
                (f, fallback_path)
            }
        };

        let lock =
            Flock::lock(file, nix::fcntl::FlockArg::LockExclusive).map_err(|(_, errno)| {
                DriverError::Transport(format!("Failed to acquire flock: {errno}"))
            })?;

        Ok(Self {
            lock: Some(lock),
            path,
        })
    }

    /// Attempts to acquire an exclusive advisory file lock on `/run/lock/pixma-g2010.lock`,
    /// falling back to a verified per-user `0700` directory if `/run/lock` is non-writable or absent.
    pub fn acquire() -> Result<Self, DriverError> {
        let uid = Self::current_uid();
        let fallback_dir = std::env::temp_dir().join(format!("pixma-lock-{uid}"));
        Self::acquire_with_paths(Path::new("/run/lock/pixma-g2010.lock"), &fallback_dir)
    }

    /// Explicitly releases the lock.
    pub fn release(mut self) -> Result<(), DriverError> {
        if let Some(lock) = self.lock.take() {
            lock.unlock().map_err(|(_, errno)| {
                DriverError::Transport(format!("Failed to unlock flock: {errno}"))
            })?;
        }
        Ok(())
    }

    /// Returns the filesystem path of the acquired lock file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Enumerates connected Canon PIXMA G-series devices over USB.
///
/// Filters for Canon Vendor ID (`0x04A9`) and known G-series Product IDs,
/// returning each model paired with its serial number or bus identifier.
pub fn discover_canon_devices() -> Result<Vec<(PrinterModel, String)>, DriverError> {
    let list = nusb::list_devices()
        .map_err(|e| DriverError::Transport(format!("Failed to enumerate USB devices: {e}")))?;

    let mut discovered = Vec::new();
    for dev in list {
        if dev.vendor_id() == CANON_USB_VID {
            if let Some(model) = PrinterModel::from_usb_pid(dev.product_id()) {
                let id = dev
                    .serial_number()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| {
                        format!("bus_{}_dev_{}", dev.bus_number(), dev.device_address())
                    });
                discovered.push((model, id));
            }
        }
    }
    Ok(discovered)
}

/// Native asynchronous USB transport for Canon PIXMA devices using `nusb`.
pub struct NusbTransport {
    model: PrinterModel,
    serial_number: String,
    bus_number: u8,
    device_address: u8,
    vendor_id: u16,
    product_id: u16,
    manufacturer: Option<String>,
    product_name: Option<String>,
    _device: nusb::Device,
    printer_iface: nusb::Interface,
    scanner_iface: Option<nusb::Interface>,
    _file_lock: Option<FileLockGuard>,
    printer_ep_out: u8,
    printer_ep_in: u8,
    scanner_ep_out: u8,
    scanner_ep_in: u8,
    scanner_ep_int: u8,
    scanner_rx_overflow: Vec<u8>,
}

impl NusbTransport {
    /// Opens the first detected Canon PIXMA device, claiming USB interfaces
    /// and detaching kernel drivers (`usblp`).
    pub fn open_first() -> Result<Self, DriverError> {
        let list = nusb::list_devices()
            .map_err(|e| DriverError::Transport(format!("Failed to list USB devices: {e}")))?;

        for dev in list {
            if dev.vendor_id() == CANON_USB_VID {
                if let Some(model) = PrinterModel::from_usb_pid(dev.product_id()) {
                    return Self::open_device(dev, model);
                }
            }
        }

        Err(DriverError::Transport(
            "No supported Canon PIXMA G-series USB device found".into(),
        ))
    }

    /// Returns the USB hardware serial number reported by the device descriptor.
    pub fn serial_number(&self) -> &str {
        &self.serial_number
    }

    /// Returns the physical USB bus/device/VID:PID/endpoints marker for this handle.
    pub fn hardware_marker(&self) -> UsbHardwareMarker {
        let endpoints_summary = if self.scanner_iface.is_some() {
            format!(
                "IF1 Print(OUT 0x{:02x}, IN 0x{:02x}) | IF0 Scan(OUT 0x{:02x}, IN 0x{:02x}, INT 0x{:02x})",
                self.printer_ep_out,
                self.printer_ep_in,
                self.scanner_ep_out,
                self.scanner_ep_in,
                self.scanner_ep_int
            )
        } else {
            format!(
                "IF1 Print(OUT 0x{:02x}, IN 0x{:02x}) | IF0 Scan(unclaimed)",
                self.printer_ep_out, self.printer_ep_in
            )
        };

        UsbHardwareMarker {
            is_mock: false,
            bus_number: Some(self.bus_number),
            device_address: Some(self.device_address),
            vendor_id: Some(self.vendor_id),
            product_id: Some(self.product_id),
            serial_number: self.serial_number.clone(),
            manufacturer: self.manufacturer.clone(),
            product_name: self.product_name.clone(),
            endpoints_summary,
        }
    }

    /// Opens a specific USB device handle, claims Printer (Interface 1) and
    /// Scanner (Interface 0, if present), and dynamically resolves endpoints.
    pub fn open_device(info: nusb::DeviceInfo, model: PrinterModel) -> Result<Self, DriverError> {
        let file_lock = match FileLockGuard::acquire() {
            Ok(guard) => Some(guard),
            Err(e) => {
                tracing::warn!("Could not acquire cooperative USB lock: {e}");
                None
            }
        };
        let serial_number = info.serial_number().unwrap_or("UNKNOWN").to_string();
        let bus_number = info.bus_number();
        let device_address = info.device_address();
        let vendor_id = info.vendor_id();
        let product_id = info.product_id();
        let manufacturer = info.manufacturer_string().map(str::to_string);
        let product_name = info.product_string().map(str::to_string);

        let device = info
            .open()
            .map_err(|e| DriverError::Transport(format!("Failed to open USB device: {e}")))?;

        // Default endpoints verified against physical Canon PIXMA G2010 (04a9:183a):
        // Interface 0 (Scanner): EP 7 OUT (0x07), EP 8 IN (0x88), EP 9 INT IN (0x89)
        // Interface 1 (Printer): EP 1 OUT (0x01), EP 2 IN (0x82)
        let mut printer_ep_out: u8 = 0x01;
        let mut printer_ep_in: u8 = 0x82;
        let mut scanner_ep_out: u8 = 0x07;
        let mut scanner_ep_in: u8 = 0x88;
        let mut scanner_ep_int: u8 = 0x89;

        if let Ok(cfg) = device.active_configuration() {
            for iface in cfg.interface_alt_settings() {
                if iface.interface_number() == 1 {
                    for ep in iface.endpoints() {
                        match (ep.transfer_type(), ep.direction()) {
                            (EndpointType::Bulk, Direction::Out) => printer_ep_out = ep.address(),
                            (EndpointType::Bulk, Direction::In) => printer_ep_in = ep.address(),
                            _ => {}
                        }
                    }
                } else if iface.interface_number() == 0 {
                    for ep in iface.endpoints() {
                        match (ep.transfer_type(), ep.direction()) {
                            (EndpointType::Bulk, Direction::Out) => scanner_ep_out = ep.address(),
                            (EndpointType::Bulk, Direction::In) => scanner_ep_in = ep.address(),
                            (EndpointType::Interrupt, Direction::In) => {
                                scanner_ep_int = ep.address()
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        // Detach kernel driver on Interface 1 (usblp) and claim it atomically
        let printer_iface = device.detach_and_claim_interface(1).map_err(|e| {
            DriverError::Transport(format!("Failed to detach/claim printer interface 1: {e}"))
        })?;

        // Claim Interface 0 (Scanner) if supported by the model
        let scanner_iface = if model.capabilities().has_scanner {
            match device.detach_and_claim_interface(0) {
                Ok(iface) => Some(iface),
                Err(err) => {
                    tracing::warn!("Could not claim scanner interface 0: {err}");
                    None
                }
            }
        } else {
            None
        };

        Ok(Self {
            model,
            serial_number,
            bus_number,
            device_address,
            vendor_id,
            product_id,
            manufacturer,
            product_name,
            _device: device,
            printer_iface,
            scanner_iface,
            _file_lock: file_lock,
            printer_ep_out,
            printer_ep_in,
            scanner_ep_out,
            scanner_ep_in,
            scanner_ep_int,
            scanner_rx_overflow: Vec::new(),
        })
    }
}

impl UsbTransport for NusbTransport {
    fn model(&self) -> PrinterModel {
        self.model
    }

    fn serial_number(&self) -> &str {
        &self.serial_number
    }

    async fn get_device_id(&mut self) -> Result<String, DriverError> {
        let ctrl = ControlIn {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request: 0x00,
            value: 0,
            index: 1,
            length: 1024,
        };

        let res = tokio::time::timeout(Duration::from_secs(3), self.printer_iface.control_in(ctrl))
            .await
            .map_err(|_| {
                DriverError::Transport("GET_DEVICE_ID control transfer timed out".into())
            })?;
        res.status
            .map_err(|e| DriverError::Transport(format!("GET_DEVICE_ID transfer failed: {e}")))?;

        let buf = &res.data;
        if buf.len() < 2 {
            return Err(DriverError::Transport(
                "Device ID response too short".into(),
            ));
        }

        let length = u16::from_be_bytes([buf[0], buf[1]]) as usize;
        // IEEE-1284 length field in some Canon firmwares includes the 2-byte length header itself
        let payload_len = if length >= 2 && length <= buf.len() {
            length - 2
        } else {
            length.min(buf.len().saturating_sub(2))
        };
        let available = buf.len().saturating_sub(2);
        let valid_len = payload_len.min(available);
        Ok(String::from_utf8_lossy(&buf[2..2 + valid_len]).to_string())
    }

    async fn get_port_status(&mut self) -> Result<u8, DriverError> {
        let ctrl = ControlIn {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request: 0x01,
            value: 0,
            index: 1,
            length: 1,
        };

        let res = tokio::time::timeout(Duration::from_secs(3), self.printer_iface.control_in(ctrl))
            .await
            .map_err(|_| DriverError::Transport("GET_PORT_STATUS timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("GET_PORT_STATUS failed: {e}")))?;

        if res.data.is_empty() {
            return Err(DriverError::Transport("Empty port status response".into()));
        }

        Ok(res.data[0])
    }

    async fn soft_reset(&mut self) -> Result<(), DriverError> {
        let ctrl = ControlOut {
            control_type: ControlType::Class,
            recipient: Recipient::Interface,
            request: 0x02,
            value: 0,
            index: 1,
            data: &[],
        };

        let res =
            tokio::time::timeout(Duration::from_secs(3), self.printer_iface.control_out(ctrl))
                .await
                .map_err(|_| DriverError::Transport("SOFT_RESET timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("SOFT_RESET failed: {e}")))?;

        Ok(())
    }

    async fn write_printer<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        let res = tokio::time::timeout(
            // Real inkjet hardware holds EP1 OUT NAK during idle wake-up priming,
            // paper feed, and dense passes when its internal RX buffer fills up.
            Duration::from_secs(180),
            self.printer_iface
                .bulk_out(self.printer_ep_out, data.to_vec()),
        )
        .await
        .map_err(|_| DriverError::Transport("Printer bulk write timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("Printer bulk write failed: {e}")))?;
        Ok(data.len())
    }

    async fn read_printer<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        let req = RequestBuffer::new(buf.len().max(512));
        let res = tokio::time::timeout(
            Duration::from_millis(250),
            self.printer_iface.bulk_in(self.printer_ep_in, req),
        )
        .await
        .map_err(|_| DriverError::Transport("Printer bulk read timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("Printer bulk read failed: {e}")))?;

        let n = res.data.len().min(buf.len());
        buf[..n].copy_from_slice(&res.data[..n]);
        Ok(n)
    }

    async fn write_scanner<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        let iface = self.scanner_iface.as_ref().ok_or_else(|| {
            DriverError::Transport("Scanner interface not available on this model".into())
        })?;

        let res = tokio::time::timeout(
            Duration::from_secs(15),
            iface.bulk_out(self.scanner_ep_out, data.to_vec()),
        )
        .await
        .map_err(|_| DriverError::Transport("Scanner bulk write timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("Scanner bulk write failed: {e}")))?;
        Ok(data.len())
    }

    async fn read_scanner<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        let iface = self.scanner_iface.as_ref().ok_or_else(|| {
            DriverError::Transport("Scanner interface not available on this model".into())
        })?;
        let ep_in = self.scanner_ep_in;

        read_scanner_with_overflow(&mut self.scanner_rx_overflow, buf, |aligned_len| async move {
            let req = RequestBuffer::new(aligned_len);
            let res =
                tokio::time::timeout(Duration::from_secs(35), iface.bulk_in(ep_in, req))
                    .await
                    .map_err(|_| DriverError::Transport("Scanner bulk read timed out".into()))?;
            res.status
                .map_err(|e| DriverError::Transport(format!("Scanner bulk read failed: {e}")))?;
            Ok(res.data)
        })
        .await
    }

    async fn poll_interrupt(&mut self) -> Result<Option<u8>, DriverError> {
        let iface = match self.scanner_iface.as_ref() {
            Some(i) => i,
            None => return Ok(None),
        };

        let req = RequestBuffer::new(64);
        let res = tokio::time::timeout(
            Duration::from_millis(500),
            iface.interrupt_in(self.scanner_ep_int, req),
        )
        .await;
        if let Ok(completion) = res {
            if completion.status.is_ok() {
                if let Some(&byte) = completion.data.first() {
                    return Ok(Some(byte));
                }
            }
        }
        Ok(None)
    }

    fn clear_scanner_rx_overflow(&mut self) {
        self.scanner_rx_overflow.clear();
    }
}

/// Drains buffered trailing USB bytes from prior 512-byte aligned reads before issuing a new bulk IN.
async fn read_scanner_with_overflow<F, Fut>(
    overflow: &mut Vec<u8>,
    buf: &mut [u8],
    read_fn: F,
) -> Result<usize, DriverError>
where
    F: FnOnce(usize) -> Fut,
    Fut: std::future::Future<Output = Result<Vec<u8>, DriverError>>,
{
    if buf.is_empty() {
        return Ok(0);
    }
    if !overflow.is_empty() {
        let n = overflow.len().min(buf.len());
        buf[..n].copy_from_slice(&overflow[..n]);
        overflow.drain(..n);
        return Ok(n);
    }

    let aligned_len = buf.len().max(512).div_ceil(512) * 512;
    let data = read_fn(aligned_len).await?;
    let n = data.len().min(buf.len());
    buf[..n].copy_from_slice(&data[..n]);
    if data.len() > n {
        overflow.extend_from_slice(&data[n..]);
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static TEST_SEQ: AtomicU64 = AtomicU64::new(0);

    fn unique_temp_root() -> PathBuf {
        let seq = TEST_SEQ.fetch_add(1, Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let root = std::env::temp_dir().join(format!(
            "pixma-nusb-test-{}-{}-{nanos}",
            std::process::id(),
            seq
        ));
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn file_lock_guard_uses_0664_primary_and_readonly_same_inode_fallback() {
        let root = unique_temp_root();
        let primary = root.join("primary.lock");
        let fallback_dir = root.join("fallback-lock");

        let guard = FileLockGuard::acquire_with_paths(&primary, &fallback_dir)
            .expect("Primary lock creation should succeed");
        assert_eq!(guard.path(), primary);
        let mode = std::fs::metadata(&primary).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o664);
        guard.release().unwrap();

        // Simulate read-only existing /run/lock/pixma-g2010.lock (non-lp desktop user)
        std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o444)).unwrap();
        let guard2 = FileLockGuard::acquire_with_paths(&primary, &fallback_dir)
            .expect("Read-only same-inode fallback should succeed");
        assert_eq!(guard2.path(), primary);

        // Verify exclusive lock actually blocks non-blocking second open on the same inode
        let second_fd = std::fs::OpenOptions::new().read(true).open(&primary).unwrap();
        assert!(
            Flock::lock(second_fd, nix::fcntl::FlockArg::LockExclusiveNonblock).is_err(),
            "Read-only primary lock fd must enforce exclusive flock"
        );
        guard2.release().unwrap();

        let _ = std::fs::set_permissions(&primary, std::fs::Permissions::from_mode(0o644));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn file_lock_guard_fallback_rejects_symlink_and_enforces_0700() {
        let root = unique_temp_root();
        let missing_primary = root.join("nonexistent-dir").join("pixma-g2010.lock");

        // Case 1: valid fallback creates 0700 private dir and 0600 file
        let fallback_dir = root.join("private-dir");
        let guard = FileLockGuard::acquire_with_paths(&missing_primary, &fallback_dir)
            .expect("Fallback lock creation should succeed");
        assert_eq!(guard.path(), fallback_dir.join("pixma-g2010.lock"));
        assert_eq!(
            std::fs::metadata(&fallback_dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        guard.release().unwrap();

        // Case 2: symlink fallback dir must be rejected
        let symlink_target = root.join("target-dir");
        std::fs::create_dir_all(&symlink_target).unwrap();
        let symlink_fallback = root.join("symlink-dir");
        std::os::unix::fs::symlink(&symlink_target, &symlink_fallback).unwrap();
        let err = FileLockGuard::acquire_with_paths(&missing_primary, &symlink_fallback)
            .err()
            .expect("Symlink fallback directory must be rejected");
        assert!(err.to_string().contains("Unsafe fallback lock path"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[tokio::test]
    async fn scanner_rx_overflow_preserves_trailing_usb_bytes_across_reads() {
        let mut overflow = Vec::new();
        let payload: Vec<u8> = (0..64u8).collect();

        // Read 1: caller asks for 10 bytes, USB returns 64 bytes (aligned to 512 minimum request)
        let mut buf1 = [0u8; 10];
        let mut requested_len = 0usize;
        let payload_clone = payload.clone();
        let n1 = read_scanner_with_overflow(&mut overflow, &mut buf1, |aligned| {
            requested_len = aligned;
            async move { Ok(payload_clone) }
        })
        .await
        .unwrap();
        assert_eq!(requested_len, 512);
        assert_eq!(n1, 10);
        assert_eq!(&buf1, &payload[..10]);
        assert_eq!(overflow.len(), 54);

        // Read 2 & 3: caller drains remaining buffered bytes without touching read_fn
        let mut buf2 = [0u8; 30];
        let n2 = read_scanner_with_overflow(&mut overflow, &mut buf2, |_| async {
            panic!("USB bulk_in should not be called while overflow buffer is non-empty");
        })
        .await
        .unwrap();
        assert_eq!(n2, 30);
        assert_eq!(&buf2, &payload[10..40]);

        let mut buf3 = [0u8; 64];
        let n3 = read_scanner_with_overflow(&mut overflow, &mut buf3, |_| async {
            panic!("USB bulk_in should not be called while overflow buffer is non-empty");
        })
        .await
        .unwrap();
        assert_eq!(n3, 24);
        assert_eq!(&buf3[..n3], &payload[40..64]);
        assert!(overflow.is_empty());

        // Read 4: now overflow is empty so read_fn is called again
        let mut buf4 = [0u8; 8];
        let n4 = read_scanner_with_overflow(&mut overflow, &mut buf4, |_| async {
            Ok(vec![0xAA, 0xBB])
        })
        .await
        .unwrap();
        assert_eq!(n4, 2);
        assert_eq!(&buf4[..2], &[0xAA, 0xBB]);
    }
}
