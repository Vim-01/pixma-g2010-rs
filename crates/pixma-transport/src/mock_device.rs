//! In-memory hardware emulator for the Canon PIXMA G2010 multifunction printer.
//!
//! Simulates IEEE-1284 control requests, IVEC XML job lifecycle and status queries,
//! maintenance command recording, and binary MP2 scanner parameter configuration
//! and synthetic image chunk generation.

use crate::traits::UsbTransport;
use pixma_core::errors::{DriverError, SupportCodeInfo};
use pixma_core::models::{DeviceStatus, OperationalState, PrinterModel};
use std::collections::VecDeque;

/// Standard IEEE-1284 Device ID string reported by physical Canon PIXMA G2010 units.
pub const G2010_IEEE1284_DEVICE_ID: &str =
    "MFG:Canon;CMD:BJRaster3,NCCe,IVEC;SOJ:CHMP;MDL:G2010 series;CLS:PRINTER;DES:Canon G2010 series;VER:1.020;STA:10;CID:CA_IVEC1TYPE4_IJP;";

/// Stateful virtual G2010 device simulator.
pub struct MockG2010Transport {
    model: PrinterModel,
    status: DeviceStatus,
    port_status: u8,
    printer_rx_queue: VecDeque<u8>,
    scanner_rx_queue: VecDeque<u8>,
    interrupt_queue: VecDeque<u8>,
    recorded_ivec_ops: Vec<String>,
    recorded_maintenance_ops: Vec<String>,
    recorded_printer_bytes: Vec<Vec<u8>>,
    recorded_scanner_bytes: Vec<Vec<u8>>,
    // Scanner simulation state
    scan_width: u32,
    scan_height: u32,
    scan_dpi: u32,
    scan_bpp: u8,
    scan_lines_sent: u32,
    lamp_on: bool,
    inject_code_after_page: Option<(String, usize)>,
    pages_seen: usize,
}

impl Default for MockG2010Transport {
    fn default() -> Self {
        Self::new()
    }
}

impl MockG2010Transport {
    /// Creates a new virtual G2010 emulator in an idle, ready state.
    pub fn new() -> Self {
        let status = DeviceStatus {
            model: PrinterModel::G2010,
            state: OperationalState::Idle,
            status_detail: "idle".to_string(),
            ..Default::default()
        };

        Self {
            model: PrinterModel::G2010,
            status,
            port_status: 0x18, // Select (bit 4) + Not Error (bit 3)
            printer_rx_queue: VecDeque::new(),
            scanner_rx_queue: VecDeque::new(),
            interrupt_queue: VecDeque::new(),
            recorded_ivec_ops: Vec::new(),
            recorded_maintenance_ops: Vec::new(),
            recorded_printer_bytes: Vec::new(),
            recorded_scanner_bytes: Vec::new(),
            scan_width: 0,
            scan_height: 0,
            scan_dpi: 600,
            scan_bpp: 24,
            scan_lines_sent: 0,
            lamp_on: false,
            inject_code_after_page: None,
            pages_seen: 0,
        }
    }

    /// Simulates hardware fault injection by setting or clearing a Canon support code.
    pub fn inject_support_code(&mut self, code: Option<&str>) {
        if let Some(c) = code {
            self.status.support_code = Some(c.to_string());
            if let Some(info) = SupportCodeInfo::lookup(c) {
                self.status.lcd_code = Some(info.lcd_code.to_string());
                self.status.state = if info.is_fatal_service_error {
                    OperationalState::ServiceError
                } else {
                    OperationalState::OperatorCall
                };
            }
        } else {
            self.status.support_code = None;
            self.status.lcd_code = None;
            self.status.state = OperationalState::Idle;
        }
    }

    /// Schedules an automatic support code injection after the printer processes `pages` pages.
    pub fn inject_support_code_after_pages(&mut self, code: &str, pages: usize) {
        self.inject_code_after_page = Some((code.to_string(), pages));
    }

    /// Sets the simulated waste ink absorber saturation level (`0.0..=100.0%`).
    pub fn set_waste_absorber_percent(&mut self, pct: f32) {
        self.status.waste_absorber_percent = pct;
    }

    /// Injects a front-panel button interrupt event code.
    pub fn inject_interrupt_event(&mut self, event: u8) {
        self.interrupt_queue.push_back(event);
    }

    /// Returns a slice of all recorded IVEC XML operation names.
    pub fn recorded_ivec_operations(&self) -> &[String] {
        &self.recorded_ivec_ops
    }

    /// Returns a slice of all recorded maintenance operation strings.
    pub fn recorded_maintenance_operations(&self) -> &[String] {
        &self.recorded_maintenance_ops
    }

    /// Returns all raw data payloads written to the printer endpoint.
    pub fn recorded_printer_writes(&self) -> &[Vec<u8>] {
        &self.recorded_printer_bytes
    }

    /// Returns all raw data payloads written to the scanner endpoint.
    pub fn recorded_scanner_writes(&self) -> &[Vec<u8>] {
        &self.recorded_scanner_bytes
    }

    /// Returns a reference to the internal simulated telemetry state.
    pub fn simulated_status(&self) -> &DeviceStatus {
        &self.status
    }

    /// Returns whether the simulated scanner optical lamp is active.
    pub fn is_lamp_on(&self) -> bool {
        self.lamp_on
    }

    /// Generates valid IVEC XML status response matching `parse_status_response`.
    fn generate_status_xml(&self) -> String {
        let (status_str, detail_str) = match self.status.state {
            OperationalState::Idle => ("idle", "idle"),
            OperationalState::Printing => ("processing", "printing"),
            OperationalState::Scanning => ("processing", "scanning"),
            OperationalState::Cleaning => ("processing", "cleaning"),
            OperationalState::Testing => ("processing", "testing"),
            OperationalState::Waiting => ("processing", "waiting"),
            OperationalState::CoverOpen => ("operator_call", "coveropen"),
            OperationalState::PaperOut => ("operator_call", "paperout"),
            OperationalState::PaperJam => ("operator_call", "paperjam"),
            OperationalState::OperatorCall => ("operator_call", "waiting"),
            OperationalState::ServiceError => ("support_error", "waiting"),
            OperationalState::Offline => ("stopped", "waiting"),
        };

        let support_code = self.status.support_code.as_deref().unwrap_or("0000");

        format!(
            r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:response>OK</ivec:response>
    <ivec:status>{status_str}</ivec:status>
    <vcn:statusdetail>{detail_str}</vcn:statusdetail>
    <vcn:supportcode>{support_code}</vcn:supportcode>
    <vcn:serialnumber>{serial}</vcn:serialnumber>
    <vcn:firmwareversion>{firmware}</vcn:firmwareversion>
    <vcn:model>{model}</vcn:model>
    <vcn:ink_bk>{bk}</vcn:ink_bk>
    <vcn:ink_c>{c}</vcn:ink_c>
    <vcn:ink_m>{m}</vcn:ink_m>
    <vcn:ink_y>{y}</vcn:ink_y>
    <vcn:wastetank>{waste:.1}</vcn:wastetank>
  </ivec:contents>
</cmd>"#,
            serial = self.status.serial_number,
            firmware = self.status.firmware_version,
            model = self.status.model.model_series_name(),
            bk = self.status.ink_levels.pigment_black_percent,
            c = self.status.ink_levels.cyan_percent,
            m = self.status.ink_levels.magenta_percent,
            y = self.status.ink_levels.yellow_percent,
            waste = self.status.waste_absorber_percent,
        )
    }

    /// Enqueues standard IVEC `<ivec:response>OK</ivec:response>` into printer RX queue.
    fn queue_printer_ok(&mut self) {
        let ok_xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
  <ivec:contents>
    <ivec:response>OK</ivec:response>
  </ivec:contents>
</cmd>"#;
        self.printer_rx_queue.extend(ok_xml.as_bytes());
    }

    /// Enqueues standard IVEC `<ivec:response>OK</ivec:response>` into scanner RX queue.
    fn queue_scanner_ok(&mut self) {
        let ok_xml = r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
  <ivec:contents>
    <ivec:response>OK</ivec:response>
  </ivec:contents>
</cmd>"#;
        self.scanner_rx_queue.extend(ok_xml.as_bytes());
    }
}

impl UsbTransport for MockG2010Transport {
    fn model(&self) -> PrinterModel {
        self.model
    }

    async fn get_device_id(&mut self) -> Result<String, DriverError> {
        Ok(G2010_IEEE1284_DEVICE_ID.to_string())
    }

    async fn get_port_status(&mut self) -> Result<u8, DriverError> {
        Ok(self.port_status)
    }

    async fn soft_reset(&mut self) -> Result<(), DriverError> {
        self.printer_rx_queue.clear();
        self.scanner_rx_queue.clear();
        self.interrupt_queue.clear();
        Ok(())
    }

    async fn write_printer<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        self.recorded_printer_bytes.push(data.to_vec());

        let text = String::from_utf8_lossy(data);
        if text.contains("<ivec:operation>") {
            if text.contains("GetStatus") {
                self.recorded_ivec_ops.push("GetStatus".to_string());
                let xml = self.generate_status_xml();
                self.printer_rx_queue.extend(xml.as_bytes());
            } else if text.contains("Cleaning") {
                self.recorded_ivec_ops.push("Cleaning".to_string());
                self.recorded_maintenance_ops.push(text.to_string());
                self.queue_printer_ok();
            } else if text.contains("RollerCleaning") {
                self.recorded_ivec_ops.push("RollerCleaning".to_string());
                self.recorded_maintenance_ops.push(text.to_string());
                self.queue_printer_ok();
            } else if text.contains("TestPrint") {
                self.recorded_ivec_ops.push("TestPrint".to_string());
                self.recorded_maintenance_ops.push(text.to_string());
                self.queue_printer_ok();
            } else if text.contains("StartJob") {
                self.recorded_ivec_ops.push("StartJob".to_string());
                self.queue_printer_ok();
            } else if text.contains("<ivec:operation>SetJobConfiguration</ivec:operation>") {
                self.recorded_ivec_ops
                    .push("SetJobConfiguration".to_string());
                self.queue_printer_ok();
            } else if text.contains("<ivec:operation>SetConfiguration</ivec:operation>") {
                self.recorded_ivec_ops.push("SetConfiguration".to_string());
                self.queue_printer_ok();
            } else if text.contains("VendorCmd") {
                if text.contains("SetJobConfiguration") {
                    self.recorded_ivec_ops
                        .push("SetJobConfiguration".to_string());
                } else if text.contains("SetPageConfiguration") {
                    self.recorded_ivec_ops
                        .push("SetPageConfiguration".to_string());
                    self.pages_seen += 1;
                    if let Some((ref code, target_pages)) = self.inject_code_after_page.clone() {
                        if self.pages_seen >= target_pages {
                            self.inject_support_code(Some(code));
                        }
                    }
                } else if text.contains("SetAlignmentPattern") {
                    self.recorded_ivec_ops
                        .push("SetAlignmentPattern".to_string());
                    self.recorded_maintenance_ops.push(text.to_string());
                } else {
                    self.recorded_ivec_ops.push("VendorCmd".to_string());
                }
                self.queue_printer_ok();
            } else if text.contains("SendData") {
                self.recorded_ivec_ops.push("SendData".to_string());
                self.queue_printer_ok();
            } else if text.contains("EndJob") {
                self.recorded_ivec_ops.push("EndJob".to_string());
                self.queue_printer_ok();
            } else if text.contains("CancelJob") {
                self.recorded_ivec_ops.push("CancelJob".to_string());
                self.queue_printer_ok();
            } else {
                self.queue_printer_ok();
            }
        } else {
            // Raw binary stream (e.g. BJL sync or PWG raster chunk)
            self.queue_printer_ok();
        }

        Ok(data.len())
    }

    async fn read_printer<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        let n = buf.len().min(self.printer_rx_queue.len());
        for slot in buf.iter_mut().take(n) {
            if let Some(b) = self.printer_rx_queue.pop_front() {
                *slot = b;
            }
        }
        Ok(n)
    }

    async fn write_scanner<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        self.recorded_scanner_bytes.push(data.to_vec());

        let text = String::from_utf8_lossy(data);
        if text.contains("<ivec:operation>") {
            if text.contains("StartJob") {
                self.recorded_ivec_ops.push("ScanStartJob".to_string());
                self.queue_scanner_ok();
            } else if text.contains("ModeShift") {
                self.recorded_ivec_ops.push("ScanModeShift".to_string());
                self.queue_scanner_ok();
            } else if text.contains("EndJob") {
                self.recorded_ivec_ops.push("ScanEndJob".to_string());
                self.queue_scanner_ok();
            } else {
                self.queue_scanner_ok();
            }
            return Ok(data.len());
        }

        // Binary MP2 command parsing
        if data.len() >= 2 {
            let opcode = u16::from_be_bytes([data[0], data[1]]);
            match opcode {
                0xF320 => {
                    // GetDataStatus -> 20-byte response
                    let mut resp = [0u8; 20];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    resp[16..18].copy_from_slice(&600u16.to_be_bytes());
                    resp[18..20].copy_from_slice(&600u16.to_be_bytes());
                    self.scanner_rx_queue.extend(resp);
                }
                0xDA20 => {
                    // GetCalibrationStatus -> 16-byte response (resp[8] = 0x02 ready)
                    let mut resp = [0u8; 16];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    resp[8] = 0x02;
                    resp[15] = 0xFE;
                    self.scanner_rx_queue.extend(resp);
                }
                0xDC20 => {
                    // GetOpuMode -> 16-byte response
                    let mut resp = [0u8; 16];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
                0xD520 => {
                    // SetLamp -> 8-byte response
                    self.lamp_on = data.get(3).copied().unwrap_or(0) != 0;
                    let mut resp = [0u8; 8];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
                0xD820 => {
                    // SetScanParameter (72 bytes)
                    if data.len() >= 0x2E {
                        self.scan_dpi =
                            (u16::from_be_bytes([data[0x18], data[0x19]]) & 0x7FFF) as u32;
                        self.scan_width = u32::from_be_bytes(data[0x24..0x28].try_into().unwrap());
                        self.scan_height = u32::from_be_bytes(data[0x28..0x2C].try_into().unwrap());
                        self.scan_bpp = data[0x2D];
                        self.scan_lines_sent = 0;
                    }
                    let mut resp = [0u8; 8];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
                0xD920 => {
                    // StartScan -> 8-byte response
                    let mut resp = [0u8; 8];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
                0xD420 => {
                    // ReadScanChunk -> generates synthetic scanline chunks
                    let remaining_lines = self.scan_height.saturating_sub(self.scan_lines_sent);
                    let lines_in_chunk = remaining_lines.min(16);
                    let bpp = (self.scan_bpp as usize / 8).max(1);
                    let bytes_per_line = (self.scan_width as usize) * bpp;
                    let payload_len = (lines_in_chunk as usize) * bytes_per_line;

                    self.scan_lines_sent += lines_in_chunk;
                    let is_complete = self.scan_lines_sent >= self.scan_height;

                    let mut chunk = vec![0u8; 16 + payload_len];
                    chunk[0] = 0x06;
                    chunk[1] = 0x06;
                    if is_complete {
                        chunk[2] = 0x28; // Scan complete flag
                        chunk[3] = 0x28;
                    }
                    chunk[12..16].copy_from_slice(&(payload_len as u32).to_be_bytes());

                    // Generate deterministic test pattern scanlines
                    for y_idx in 0..lines_in_chunk {
                        let line_offset = 16 + (y_idx as usize) * bytes_per_line;
                        let line_y = self.scan_lines_sent - lines_in_chunk + y_idx;
                        for x in 0..self.scan_width {
                            let px_offset = line_offset + (x as usize) * bpp;
                            if bpp == 3 {
                                chunk[px_offset] = ((x * 255) / self.scan_width.max(1)) as u8;
                                chunk[px_offset + 1] =
                                    ((line_y * 255) / self.scan_height.max(1)) as u8;
                                chunk[px_offset + 2] = 128;
                            } else {
                                chunk[px_offset] = ((x + line_y) % 256) as u8;
                            }
                        }
                    }

                    self.scanner_rx_queue.extend(chunk);
                }
                0xEF20 => {
                    // CancelOrFinishScan -> 8-byte response
                    let mut resp = [0u8; 8];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
                _ => {
                    let mut resp = [0u8; 8];
                    resp[0] = 0x06;
                    resp[1] = 0x06;
                    self.scanner_rx_queue.extend(resp);
                }
            }
        }

        Ok(data.len())
    }

    async fn read_scanner<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        let n = buf.len().min(self.scanner_rx_queue.len());
        for slot in buf.iter_mut().take(n) {
            if let Some(b) = self.scanner_rx_queue.pop_front() {
                *slot = b;
            }
        }
        Ok(n)
    }

    async fn poll_interrupt(&mut self) -> Result<Option<u8>, DriverError> {
        Ok(self.interrupt_queue.pop_front())
    }
}
