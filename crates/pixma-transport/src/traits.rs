//! Abstract USB transport trait for Canon PIXMA G-series printers.
//!
//! Provides asynchronous primitives for control requests, printer bulk pipes,
//! scanner bulk pipes, and front-panel interrupt event monitoring.

use pixma_core::errors::DriverError;
use pixma_core::models::PrinterModel;
use std::future::Future;

/// Abstract asynchronous USB transport for Canon PIXMA multifunction devices.
pub trait UsbTransport: Send + Sync {
    /// Returns the detected or configured hardware printer model.
    fn model(&self) -> PrinterModel;

    /// Returns the USB hardware serial number reported by the device descriptor.
    fn serial_number(&self) -> &str {
        "UNKNOWN"
    }

    /// Queries the IEEE-1284 Device ID string via control endpoint 0
    /// (`bmRequestType = 0xA1, bRequest = 0x00`).
    fn get_device_id(&mut self) -> impl Future<Output = Result<String, DriverError>> + Send;

    /// Queries the 1-byte printer port status register via control endpoint 0
    /// (`bmRequestType = 0xA1, bRequest = 0x01`).
    fn get_port_status(&mut self) -> impl Future<Output = Result<u8, DriverError>> + Send;

    /// Issues a soft reset to the printer interface via control endpoint 0
    /// (`bmRequestType = 0x21, bRequest = 0x02`).
    fn soft_reset(&mut self) -> impl Future<Output = Result<(), DriverError>> + Send;

    /// Streams raw bytes to the printer bulk OUT endpoint (Interface 1).
    fn write_printer<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> impl Future<Output = Result<usize, DriverError>> + Send + 'a;

    /// Reads incoming bytes from the printer bulk IN endpoint (Interface 1).
    fn read_printer<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<usize, DriverError>> + Send + 'a;

    /// Streams raw bytes to the scanner bulk OUT endpoint (Interface 0).
    fn write_scanner<'a>(
        &'a mut self,
        data: &'a [u8],
    ) -> impl Future<Output = Result<usize, DriverError>> + Send + 'a;

    /// Reads incoming bytes from the scanner bulk IN endpoint (Interface 0).
    fn read_scanner<'a>(
        &'a mut self,
        buf: &'a mut [u8],
    ) -> impl Future<Output = Result<usize, DriverError>> + Send + 'a;

    /// Polls the scanner interrupt IN endpoint (Interface 0, EP `0x83`) for
    /// front-panel button events. Returns `Ok(None)` if no event is pending.
    fn poll_interrupt(&mut self) -> impl Future<Output = Result<Option<u8>, DriverError>> + Send;
}
