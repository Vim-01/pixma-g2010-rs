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
    PrinterModel, CANON_USB_VID, G1010_USB_PID, G2000_USB_PID, G2010_USB_PID, G2020_USB_PID,
    G3000_USB_PID, G3010_USB_PID, G4000_USB_PID, G4010_USB_PID,
};
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
    /// Attempts to acquire an exclusive advisory file lock on `/run/lock/pixma-g2010.lock`,
    /// falling back to `/tmp/pixma-g2010.lock` if `/run/lock` is non-writable or absent.
    pub fn acquire() -> Result<Self, DriverError> {
        let primary_path = Path::new("/run/lock/pixma-g2010.lock");
        let fallback_path = Path::new("/tmp/pixma-g2010.lock");

        let (file, path) = match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(primary_path)
        {
            Ok(f) => (f, primary_path.to_path_buf()),
            Err(_) => {
                let f = std::fs::OpenOptions::new()
                    .read(true)
                    .write(true)
                    .create(true)
                    .truncate(false)
                    .open(fallback_path)
                    .map_err(DriverError::Io)?;
                (f, fallback_path.to_path_buf())
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
    _device: nusb::Device,
    printer_iface: nusb::Interface,
    scanner_iface: Option<nusb::Interface>,
    _file_lock: Option<FileLockGuard>,
    printer_ep_out: u8,
    printer_ep_in: u8,
    scanner_ep_out: u8,
    scanner_ep_in: u8,
    scanner_ep_int: u8,
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

    /// Opens a specific USB device handle, claims Printer (Interface 1) and
    /// Scanner (Interface 0, if present), and dynamically resolves endpoints.
    pub fn open_device(info: nusb::DeviceInfo, model: PrinterModel) -> Result<Self, DriverError> {
        let file_lock = FileLockGuard::acquire().ok();
        let serial_number = info.serial_number().unwrap_or("UNKNOWN").to_string();

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
            _device: device,
            printer_iface,
            scanner_iface,
            _file_lock: file_lock,
            printer_ep_out,
            printer_ep_in,
            scanner_ep_out,
            scanner_ep_in,
            scanner_ep_int,
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
            Duration::from_secs(15),
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

        let aligned_len = buf.len().max(512).div_ceil(512) * 512;
        let req = RequestBuffer::new(aligned_len);
        let res = tokio::time::timeout(
            Duration::from_secs(35),
            iface.bulk_in(self.scanner_ep_in, req),
        )
        .await
        .map_err(|_| DriverError::Transport("Scanner bulk read timed out".into()))?;
        res.status
            .map_err(|e| DriverError::Transport(format!("Scanner bulk read failed: {e}")))?;

        let n = res.data.len().min(buf.len());
        buf[..n].copy_from_slice(&res.data[..n]);
        Ok(n)
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
}
