//! High-level hardware-safe controller for Canon PIXMA G-series devices.
//!
//! Orchestrates the complete print job lifecycle, thermal duty-cycle throttling,
//! maintenance cooldown authorization, and MP2 flatbed scan acquisition.

use crate::mock_device::MockG2010Transport;
use crate::nusb_transport::NusbTransport;
use crate::traits::UsbTransport;
use pixma_core::errors::{DriverError, SafetyInterlockError, SupportCodeInfo};
use pixma_core::maintenance::{MaintenanceAction, MaintenanceSupervisor};
use pixma_core::models::{DeviceStatus, OperationalState, PrinterModel, UsbHardwareMarker};
use pixma_core::print_params::PrintJobConfig;
use pixma_core::safety::{PrintJobTypestate, ValidatedRasterLine};
use pixma_core::scan_params::ScanParameters;
use pixma_protocol::ivec::{
    build_cancel_job, build_cleaning_cmd, build_end_job, build_get_maintenance_capability_cmd,
    build_get_status_cmd, build_maintenance_start_job, build_manual_alignment_values_cmd,
    build_mode_shift, build_roller_cleaning_cmd, build_send_data_header,
    build_set_job_configuration, build_set_page_configuration, build_set_print_configuration,
    build_start_job, build_test_print_cmd, parse_maintenance_cleaning_groups,
    parse_status_response, parse_status_response_with_meta, ParsedStatusReport,
};
use pixma_protocol::mp2_scanner::{
    aligned_scan_width, build_cancel_or_finish_scan, build_gamma_table,
    build_get_calibration_status, build_read_scan_chunk_with_req, build_set_scan_parameter,
    build_start_scan, build_start_session, parse_scan_chunk_response,
};
use pixma_protocol::pwg_raster::{encode_pwg_page, PwgRasterHeader};

/// Unified transport enum enabling dynamic switching between physical USB (`nusb`)
/// and simulated virtual hardware (`MockG2010Transport`).
pub enum AnyTransport {
    Usb(NusbTransport),
    Mock(Box<MockG2010Transport>),
}

impl AnyTransport {
    /// Returns true if this transport wraps simulated virtual hardware.
    pub fn is_mock(&self) -> bool {
        matches!(self, Self::Mock(_))
    }

    /// Returns the physical USB bus/device/VID:PID/endpoints marker (or mock label).
    pub fn hardware_marker(&self) -> UsbHardwareMarker {
        match self {
            Self::Usb(usb) => usb.hardware_marker(),
            Self::Mock(mock) => UsbHardwareMarker {
                is_mock: true,
                bus_number: None,
                device_address: None,
                vendor_id: Some(pixma_core::models::CANON_USB_VID),
                product_id: Some(pixma_core::models::G2010_USB_PID),
                serial_number: mock.serial_number().to_string(),
                manufacturer: Some("Mock Emulator".to_string()),
                product_name: Some("Virtual G2010".to_string()),
                endpoints_summary: "Virtual USB Bulk (EP 0x01/0x82)".to_string(),
            },
        }
    }
}

/// Extracts a `KEY:VALUE;` entry from an IEEE-1284 `GET_DEVICE_ID` string.
pub fn extract_ieee1284_tag(device_id: &str, key: &str) -> Option<String> {
    for part in device_id.split(';') {
        if let Some((k, v)) = part.split_once(':') {
            if k.trim().eq_ignore_ascii_case(key) {
                let val = v.trim();
                if !val.is_empty() {
                    return Some(val.to_string());
                }
            }
        }
    }
    None
}

impl UsbTransport for AnyTransport {
    fn model(&self) -> PrinterModel {
        match self {
            Self::Usb(t) => t.model(),
            Self::Mock(t) => t.model(),
        }
    }

    fn serial_number(&self) -> &str {
        match self {
            Self::Usb(t) => t.serial_number(),
            Self::Mock(t) => t.serial_number(),
        }
    }

    fn is_mock(&self) -> bool {
        AnyTransport::is_mock(self)
    }

    async fn get_device_id(&mut self) -> Result<String, DriverError> {
        match self {
            Self::Usb(t) => t.get_device_id().await,
            Self::Mock(t) => t.get_device_id().await,
        }
    }

    async fn get_port_status(&mut self) -> Result<u8, DriverError> {
        match self {
            Self::Usb(t) => t.get_port_status().await,
            Self::Mock(t) => t.get_port_status().await,
        }
    }

    async fn soft_reset(&mut self) -> Result<(), DriverError> {
        match self {
            Self::Usb(t) => t.soft_reset().await,
            Self::Mock(t) => t.soft_reset().await,
        }
    }

    async fn write_printer<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        match self {
            Self::Usb(t) => t.write_printer(data).await,
            Self::Mock(t) => t.write_printer(data).await,
        }
    }

    async fn read_printer<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        match self {
            Self::Usb(t) => t.read_printer(buf).await,
            Self::Mock(t) => t.read_printer(buf).await,
        }
    }

    async fn write_scanner<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
        match self {
            Self::Usb(t) => t.write_scanner(data).await,
            Self::Mock(t) => t.write_scanner(data).await,
        }
    }

    async fn read_scanner<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
        match self {
            Self::Usb(t) => t.read_scanner(buf).await,
            Self::Mock(t) => t.read_scanner(buf).await,
        }
    }

    async fn poll_interrupt(&mut self) -> Result<Option<u8>, DriverError> {
        match self {
            Self::Usb(t) => t.poll_interrupt().await,
            Self::Mock(t) => t.poll_interrupt().await,
        }
    }

    fn clear_scanner_rx_overflow(&mut self) {
        match self {
            Self::Usb(t) => t.clear_scanner_rx_overflow(),
            Self::Mock(t) => t.clear_scanner_rx_overflow(),
        }
    }
}

/// Maximum size of a single MP2 scanner chunk payload (16 MiB) before rejecting corrupted headers.
const MAX_SCAN_CHUNK_BYTES: usize = 16 * 1024 * 1024;

/// Hardware-safe controller managing transport I/O, maintenance interlocks, and print/scan lifecycles.
pub struct G2010Controller<T: UsbTransport> {
    transport: T,
    supervisor: MaintenanceSupervisor,
    job_counter: u32,
    last_scan_job_id: Option<u32>,
}

impl<T: UsbTransport> G2010Controller<T> {
    /// Creates a new controller wrapping the given USB transport and maintenance supervisor.
    pub fn new(transport: T, supervisor: MaintenanceSupervisor) -> Self {
        Self {
            transport,
            supervisor,
            job_counter: 1,
            last_scan_job_id: None,
        }
    }

    /// Returns a reference to the underlying transport.
    pub fn transport(&self) -> &T {
        &self.transport
    }

    /// Returns a mutable reference to the underlying transport.
    pub fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Returns a reference to the maintenance supervisor.
    pub fn supervisor(&self) -> &MaintenanceSupervisor {
        &self.supervisor
    }

    /// Returns a mutable reference to the maintenance supervisor.
    pub fn supervisor_mut(&mut self) -> &mut MaintenanceSupervisor {
        &mut self.supervisor
    }

    /// Generates a sequential 8-digit job ID.
    fn next_job_id(&mut self) -> u32 {
        let id = self.job_counter;
        self.job_counter = self.job_counter.wrapping_add(1).max(1);
        id
    }

    /// Queries real-time hardware status and telemetry via IVEC XML (`GetStatus`),
    /// returning both the structured status and metadata showing which fields were
    /// explicitly emitted by the printer firmware.
    pub async fn query_status_report(&mut self) -> Result<ParsedStatusReport, DriverError> {
        let (report, _) = self.query_status_with_drained().await?;
        Ok(report)
    }

    /// Sends an IVEC query command and drains unrelated asynchronous job responses
    /// (`StartJobResponse`, `EndJobResponse`, etc.) ahead of the matching reply.
    async fn query_ivec_with_drained(
        &mut self,
        cmd: &str,
        buf_size: usize,
        label: &str,
        is_match: impl Fn(&ParsedStatusReport) -> bool,
    ) -> Result<(ParsedStatusReport, Vec<ParsedStatusReport>), DriverError> {
        self.transport.write_printer(cmd.as_bytes()).await?;

        let is_mock = self.transport.is_mock();
        let mut buf = vec![0u8; buf_size];
        let mut drained = Vec::new();
        let mut resent_after_drain = false;

        for attempt in 0..8 {
            let bytes_read = self.transport.read_printer(&mut buf).await?;
            if bytes_read == 0 {
                if is_mock || attempt == 7 {
                    return Err(DriverError::Transport(format!(
                        "Empty {label} reply from printer"
                    )));
                }
                if !drained.is_empty() && !resent_after_drain {
                    self.transport.write_printer(cmd.as_bytes()).await?;
                    resent_after_drain = true;
                } else {
                    tokio::time::sleep(std::time::Duration::from_millis(40)).await;
                }
                continue;
            }

            let xml = String::from_utf8_lossy(&buf[..bytes_read]);
            tracing::debug!("Raw {label} XML ({bytes_read} bytes):\n{xml}");
            let report = parse_status_response_with_meta(&xml)
                .map_err(|e| DriverError::Protocol(format!("Failed to parse {label} XML: {e}")))?;

            if !is_match(&report) {
                tracing::info!(
                    "Drained async IVEC packet during {label}: op={:?}, resp={:?}, detail={:?}",
                    report.ivec_operation,
                    report.ivec_response,
                    report.ivec_response_detail
                );
                drained.push(report);
                if attempt == 7 {
                    return Err(DriverError::Protocol(format!(
                        "Printer returned only non-matching IVEC packets to {label}"
                    )));
                }
                continue;
            }

            return Ok((report, drained));
        }

        Err(DriverError::Transport(format!(
            "Timed out waiting for {label} response from printer"
        )))
    }

    /// Queries `GetStatus`, draining any asynchronous job responses (`StartJobResponse`,
    /// `EndJobResponse`, etc.) ahead of the `GetStatusResponse` in the EP1 IN buffer.
    async fn query_status_with_drained(
        &mut self,
    ) -> Result<(ParsedStatusReport, Vec<ParsedStatusReport>), DriverError> {
        let cmd = build_get_status_cmd();
        let (mut report, drained) = self
            .query_ivec_with_drained(&cmd, 4096, "GetStatus", |r| r.is_status_response())
            .await?;

        if report.status.serial_number == "UNKNOWN" && self.transport.serial_number() != "UNKNOWN" {
            report.status.serial_number = self.transport.serial_number().to_string();
        }
        report.status.model = self.transport.model();

        if report.firmware_reported_waste {
            self.supervisor
                .update_d_counter_from_hardware(report.status.waste_absorber_percent);
        }

        Ok((report, drained))
    }

    /// Best-effort `GetCapability(servicetype="maintenance")` query used when hardware
    /// stays Idle after a maintenance command, logging the live XML and returning
    /// supported `<ivec:inkgroup>` combinations for diagnostics.
    async fn query_maintenance_capability_summary(&mut self) -> Option<String> {
        let cmd = build_get_maintenance_capability_cmd();
        let (report, _) = self
            .query_ivec_with_drained(&cmd, 16384, "GetCapability(maintenance)", |r| {
                r.is_capability_response()
            })
            .await
            .ok()?;

        tracing::info!(
            "Live firmware GetCapability(maintenance) XML ({} bytes):\n{}",
            report.raw_xml.len(),
            report.raw_xml
        );

        let groups = parse_maintenance_cleaning_groups(&report.raw_xml).ok()?;
        if groups.is_empty() {
            return None;
        }
        let summary = groups
            .into_iter()
            .map(|(kind, ink_groups)| format!("{kind}=[{}]", ink_groups.join(", ")))
            .collect::<Vec<_>>()
            .join(", ");
        Some(format!("firmware allows: {summary}"))
    }

    fn validate_ivec_reply_ok(report: &ParsedStatusReport) -> Result<(), DriverError> {
        if let Some(ref resp) = report.ivec_response {
            if matches!(
                resp.trim().to_ascii_uppercase().as_str(),
                "NG" | "ERROR" | "FAILED"
            ) {
                let op = report.ivec_operation.as_deref().unwrap_or("UnknownOp");
                let detail = report
                    .ivec_response_detail
                    .as_deref()
                    .filter(|s| !s.is_empty())
                    .unwrap_or("no details");
                return Err(DriverError::Protocol(format!(
                    "Printer rejected command {op}: {resp} ({detail})"
                )));
            }
        }
        Ok(())
    }

    async fn write_and_drain_optional_reply(&mut self, data: &[u8]) -> Result<(), DriverError> {
        self.transport.write_printer(data).await?;
        let mut rx_buf = [0u8; 1024];
        if let Ok(n) = self.transport.read_printer(&mut rx_buf).await {
            if n > 0 {
                let xml = String::from_utf8_lossy(&rx_buf[..n]);
                tracing::debug!("Maintenance step IVEC reply ({n} bytes):\n{xml}");
                if let Ok(report) = parse_status_response_with_meta(&xml) {
                    Self::validate_ivec_reply_ok(&report)?;
                }
            }
        }
        Ok(())
    }

    /// Queries real-time hardware status and telemetry via IVEC XML (`GetStatus`).
    ///
    /// Updates the maintenance supervisor's estimated D-counter using reported values.
    pub async fn query_status(&mut self) -> Result<DeviceStatus, DriverError> {
        self.query_status_report().await.map(|report| report.status)
    }

    /// Queries IEEE-1284 `GET_DEVICE_ID` and extracts a `KEY:VALUE;` tag if present.
    pub async fn query_ieee1284_info(&mut self) -> Result<String, DriverError> {
        self.transport.get_device_id().await
    }

    /// Queries IEEE-1284 USB `GET_PORT_STATUS` byte (bit 5=Paper Empty, bit 4=Select, bit 3=Not Error).
    pub async fn query_port_status(&mut self) -> Result<u8, DriverError> {
        self.transport.get_port_status().await
    }

    /// Executes an authorized maintenance operation after enforcing safety cooldowns and D-counter limits.
    pub async fn execute_maintenance(
        &mut self,
        action: MaintenanceAction,
        confirmation_token: Option<&str>,
        override_cooldown: bool,
    ) -> Result<DeviceStatus, DriverError> {
        let now = MaintenanceSupervisor::current_unix_epoch();

        if let MaintenanceAction::Clean { kind, .. } = action {
            self.supervisor
                .authorize_cleaning(kind, confirmation_token, override_cooldown, now)?;
        }

        let initial_status = self.query_status().await?;
        if let Some(ref code) = initial_status.support_code {
            let lcd = initial_status.lcd_code.as_deref().unwrap_or("—");
            return Err(DriverError::Transport(format!(
                "Printer reported active support code {code} (LCD: {lcd}) before starting maintenance"
            )));
        }

        let is_mock = self.transport.is_mock();
        let job_id = self.next_job_id();

        // Canon cmdtocanonij3 / IVEC maintenance job framing (verified on PIXMA G-series):
        // StartJob("maintenance", bidi=0) -> NUL padding (len == StartJob.len()) -> Operation XML -> EndJob("maintenance")
        let start_job = build_maintenance_start_job(job_id);
        let nul_padding = vec![0u8; start_job.len()];
        let op_cmd = match action {
            MaintenanceAction::Clean { kind, target } => build_cleaning_cmd(job_id, kind, target),
            MaintenanceAction::MechanicalClean(target) => {
                build_roller_cleaning_cmd(job_id, target)
            }
            MaintenanceAction::TestPrint(test_type) => build_test_print_cmd(job_id, test_type),
            MaintenanceAction::ApplyManualAlignment { ref column_values } => {
                build_manual_alignment_values_cmd(job_id, column_values)
            }
        };
        let end_job = build_end_job("maintenance", job_id);

        tracing::info!(
            "Dispatching IVEC maintenance action {action:?} (job_id={job_id:08}, nul_pad={})",
            nul_padding.len()
        );
        self.write_and_drain_optional_reply(start_job.as_bytes())
            .await?;
        self.write_and_drain_optional_reply(&nul_padding).await?;
        self.write_and_drain_optional_reply(op_cmd.as_bytes())
            .await?;
        self.write_and_drain_optional_reply(end_job.as_bytes())
            .await?;

        if is_mock {
            let status = self.query_status().await?;
            if let MaintenanceAction::Clean { kind, .. } = action {
                if let Err(err) = self.supervisor.record_cleaning_executed(kind, now) {
                    tracing::error!(
                        "Maintenance executed on mock hardware, but failed to persist cooldown state: {err}"
                    );
                }
            }
            return Ok(status);
        }

        let expects_mechanical_activity =
            !matches!(action, MaintenanceAction::ApplyManualAlignment { .. });
        if !expects_mechanical_activity {
            return self.query_status().await;
        }

        let mut latest_status = initial_status;
        for poll_idx in 0..8 {
            if poll_idx > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
            let (report, drained) = self.query_status_with_drained().await?;
            for reply in &drained {
                Self::validate_ivec_reply_ok(reply)?;
            }
            latest_status = report.status;

            if let Some(ref code) = latest_status.support_code {
                let lcd = latest_status.lcd_code.as_deref().unwrap_or("—");
                return Err(DriverError::Transport(format!(
                    "Printer reported error during maintenance: code {code} (LCD: {lcd})"
                )));
            }

            let is_busy_now = matches!(
                latest_status.state,
                OperationalState::Cleaning
                    | OperationalState::Testing
                    | OperationalState::Printing
                    | OperationalState::Waiting
            ) || report.raw_status.as_deref() == Some("processing");

            if is_busy_now {
                tracing::info!(
                    "Hardware confirmed maintenance activity after {poll_idx} polls: {:?} ({})",
                    latest_status.state,
                    latest_status.status_detail
                );
                if let MaintenanceAction::Clean { kind, .. } = action {
                    if let Err(err) = self.supervisor.record_cleaning_executed(kind, now) {
                        tracing::error!(
                            "Maintenance confirmed on hardware, but failed to persist cooldown state: {err}"
                        );
                    }
                }
                return Ok(latest_status);
            }
        }

        tracing::warn!(
            "Maintenance command {action:?} left printer in {:?} ({}) without mechanical start",
            latest_status.state,
            latest_status.status_detail
        );
        let cap_suffix = self
            .query_maintenance_capability_summary()
            .await
            .map(|summary| format!(" ({summary})"))
            .unwrap_or_default();
        Err(DriverError::Transport(match action {
            MaintenanceAction::Clean { kind, target } => format!(
                "Printer remained Idle and did not start cleaning ({kind:?} / {target:?}, token '{}'){cap_suffix}.",
                target.ivec_xml_value()
            ),
            _ => format!(
                "Printer accepted packet ({action:?}) but remained Idle and did not start operation{cap_suffix}."
            ),
        }))
    }

    /// Submits a multi-page PWG raster print job, enforcing strict hardware bounds,
    /// thermal duty-cycle cooling pauses, and immediate emergency abort on printhead overheating.
    pub async fn submit_pwg_print_job(
        &mut self,
        config: &PrintJobConfig,
        pages: &[Vec<ValidatedRasterLine>],
        timestamp: &str,
    ) -> Result<(), DriverError> {
        if pages.is_empty() {
            return Ok(());
        }

        let mut rx_buf = [0u8; 1024];

        // 1. Initial status query & thermal safety interlock
        let initial_status = self.query_status().await?;
        if let Some(ref code) = initial_status.support_code {
            if SupportCodeInfo::requires_immediate_thermal_abort(code) {
                let cause = SupportCodeInfo::lookup(code)
                    .map(|i| i.cause.to_string())
                    .unwrap_or_else(|| "Printhead thermal hazard".to_string());
                let cancel = build_cancel_job("print", config.job_id);
                let _ = self.transport.write_printer(cancel.as_bytes()).await;
                let _ = self.transport.read_printer(&mut rx_buf).await;
                return Err(DriverError::SafetyInterlock(
                    SafetyInterlockError::EmergencyHardwareAbort {
                        support_code: code.clone(),
                        description: cause,
                    },
                ));
            }
        }

        // 2. StartJob (must precede all job configuration in Canon IVEC protocol)
        let start_job = build_start_job("print", config.job_id);
        self.transport.write_printer(start_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        // 3. SetJobConfiguration
        let set_job = build_set_job_configuration(config.job_id, Some(timestamp));
        self.transport.write_printer(set_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        // 4. SetConfiguration (papersize, papertype, borderless, colormode, duplex)
        let set_config = build_set_print_configuration(config);
        self.transport.write_printer(set_config.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        // 5. Stream Pages
        for (page_idx, lines) in pages.iter().enumerate() {
            let page_no = (page_idx + 1) as u32;

            // Query status before every single page
            let page_status = self.query_status().await?;
            if let Some(ref code) = page_status.support_code {
                if SupportCodeInfo::requires_immediate_thermal_abort(code) {
                    let cause = SupportCodeInfo::lookup(code)
                        .map(|i| i.cause.to_string())
                        .unwrap_or_else(|| "Printhead thermal hazard".to_string());
                    let cancel = build_cancel_job("print", config.job_id);
                    let _ = self.transport.write_printer(cancel.as_bytes()).await;
                    let _ = self.transport.read_printer(&mut rx_buf).await;
                    return Err(DriverError::SafetyInterlock(
                        SafetyInterlockError::EmergencyHardwareAbort {
                            support_code: code.clone(),
                            description: cause,
                        },
                    ));
                }
            }

            if lines.is_empty() {
                continue;
            }

            let dot_width = lines[0].dot_width();
            let height_dots = lines.len() as u32;
            let dpi = lines[0].dpi();
            let bpp = lines[0].bytes_per_pixel();

            // Typestate validation and thermal duty-cycle tracking
            let typestate = PrintJobTypestate::new();
            let page_cfg_typestate = typestate
                .configure_page(dot_width, height_dots, dpi)
                .map_err(DriverError::HardwareConstraint)?;
            let mut streaming = page_cfg_typestate.begin_streaming();

            for line in lines {
                if let Some(pause) = streaming
                    .accept_line(line)
                    .map_err(DriverError::HardwareConstraint)?
                {
                    tokio::time::sleep(pause).await;
                }
            }

            // SetPageConfiguration
            let is_last_page = page_idx + 1 == pages.len();
            let page_cfg_xml = build_set_page_configuration(config, is_last_page);
            self.transport
                .write_printer(page_cfg_xml.as_bytes())
                .await?;
            let _ = self.transport.read_printer(&mut rx_buf).await;

            // PWG page encoding
            let header = if bpp == 1 {
                PwgRasterHeader::new_sgray(dot_width, height_dots, dpi)
            } else {
                PwgRasterHeader::new_srgb(dot_width, height_dots, dpi)
            };
            let pwg_data = encode_pwg_page(&header, lines);

            // SendData XML header
            let send_data_xml =
                build_send_data_header(config.job_id, page_no, "PWGRaster", pwg_data.len());
            self.transport
                .write_printer(send_data_xml.as_bytes())
                .await?;

            // Stream PWG raster payload in chunks up to 256 KB
            const CHUNK_SIZE: usize = 256 * 1024;
            for chunk in pwg_data.chunks(CHUNK_SIZE) {
                self.transport.write_printer(chunk).await?;
            }
            let _ = self.transport.read_printer(&mut rx_buf).await;
        }

        // 6. EndJob
        let end_job = build_end_job("print", config.job_id);
        self.transport.write_printer(end_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        Ok(())
    }

    /// Streams raw bytes directly to the printer interface bulk OUT pipe,
    /// then monitors the printer status until physical page execution and ejection complete.
    pub async fn print_raw_stream(&mut self, data: &[u8]) -> Result<(), DriverError> {
        const CHUNK_SIZE: usize = 16 * 1024;
        let now = MaintenanceSupervisor::current_unix_epoch();
        let raw_clean_actions = pixma_protocol::ivec::extract_ivec_cleaning_actions(data);
        if !raw_clean_actions.is_empty() {
            let mut validator =
                MaintenanceSupervisor::in_memory(self.supervisor.record().clone());
            for &kind in &raw_clean_actions {
                validator
                    .authorize_cleaning(kind, None, false, now)
                    .map_err(DriverError::SafetyInterlock)?;
                let _ = validator.record_cleaning_executed(kind, now);
            }
        }

        tracing::info!(
            "print_raw_stream: streaming {} bytes to printer bulk OUT",
            data.len()
        );
        let mut sent = 0usize;
        for chunk in data.chunks(CHUNK_SIZE) {
            if let Err(e) = self.transport.write_printer(chunk).await {
                tracing::error!(
                    "print_raw_stream: aborted at byte {sent}/{}: {e}",
                    data.len()
                );
                return Err(e);
            }
            sent += chunk.len();
            if sent % (256 * 1024) < CHUNK_SIZE || sent == data.len() {
                tracing::debug!(
                    "print_raw_stream: transferred {sent}/{} bytes",
                    data.len()
                );
            }
        }
        tracing::info!(
            "print_raw_stream: finished transferring {} bytes; waiting for hardware idle",
            data.len()
        );

        for &kind in &raw_clean_actions {
            if let Err(err) = self.supervisor.record_cleaning_executed(kind, now) {
                tracing::error!(
                    "Raw IVEC cleaning ({kind:?}) sent to hardware, but failed to persist cooldown state: {err}"
                );
            }
        }

        let is_mock = self.transport.is_mock();
        if !is_mock {
            let mut saw_printing = false;
            let mut buf = vec![0u8; 4096];
            for attempt in 0..360 {
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                let mut parsed_status = None;
                if let Ok(bytes_read) = self.transport.read_printer(&mut buf).await {
                    if bytes_read > 0 {
                        let xml = String::from_utf8_lossy(&buf[..bytes_read]);
                        if xml.contains("<ivec:status>") {
                            if let Ok(st) = parse_status_response(&xml) {
                                parsed_status = Some(st);
                            }
                        }
                    }
                }
                if parsed_status.is_none() && attempt % 2 == 1 {
                    if let Ok(st) = self.query_status().await {
                        parsed_status = Some(st);
                    }
                }
                if let Some(st) = parsed_status {
                    match st.state {
                        pixma_core::models::OperationalState::Printing
                        | pixma_core::models::OperationalState::Testing
                        | pixma_core::models::OperationalState::Cleaning => {
                            saw_printing = true;
                        }
                        pixma_core::models::OperationalState::Idle => {
                            if saw_printing || attempt >= 6 {
                                tracing::info!(
                                    "print_raw_stream: hardware returned Idle after {} attempts",
                                    attempt + 1
                                );
                                break;
                            }
                        }
                        pixma_core::models::OperationalState::PaperOut => {
                            return Err(DriverError::Protocol(
                                "Printer reported Paper Out (Support Code 1000/1003). Load paper in rear feeder.".to_string(),
                            ));
                        }
                        pixma_core::models::OperationalState::PaperJam => {
                            return Err(DriverError::Protocol(
                                "Printer reported Paper Jam (Support Code 1300). Clear paper jam.".to_string(),
                            ));
                        }
                        other => {
                            tracing::debug!(
                                "print_raw_stream: hardware state during completion poll: {other:?}"
                            );
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Parses a scanner IVEC XML command reply and verifies `<ivec:response>OK</ivec:response>`.
    fn verify_ivec_xml_ok(step: &str, bytes: &[u8]) -> Result<(), DriverError> {
        let xml = std::str::from_utf8(bytes).map_err(|_| {
            DriverError::Protocol(format!("{step} returned non-UTF-8 response"))
        })?;
        let trimmed = xml.trim_start();
        if !trimmed.starts_with('<') {
            return Err(DriverError::Protocol(format!(
                "{step} returned non-XML response: {:02X?}",
                &bytes[..bytes.len().min(16)]
            )));
        }
        let report = parse_status_response_with_meta(xml).map_err(|e| {
            DriverError::Protocol(format!("{step} XML parse failed: {e}"))
        })?;
        match report.ivec_response.as_deref() {
            Some(resp) if resp.trim().eq_ignore_ascii_case("OK") => Ok(()),
            Some(resp) => Err(DriverError::Protocol(format!(
                "{step} rejected by hardware: response={resp} detail={}",
                report.ivec_response_detail.as_deref().unwrap_or("")
            ))),
            None => Err(DriverError::Protocol(format!(
                "{step} XML missing <ivec:response>"
            ))),
        }
    }

    /// Best-effort bounded read used ONLY during error/recovery cleanup so orphaned states
    /// do not block 35s. Clears any stale local overflow before/after probing USB.
    async fn drain_scanner_optional(&mut self, timeout: std::time::Duration) {
        self.transport.clear_scanner_rx_overflow();
        let mut buf = [0u8; 1024];
        let res = tokio::time::timeout(timeout, self.transport.read_scanner(&mut buf)).await;
        if res.is_err() {
            tracing::warn!("drain_scanner_optional: recovery read timed out after {timeout:?}");
        }
        self.transport.clear_scanner_rx_overflow();
    }

    /// Cleanly exits binary MP2 mode (if active) and terminates an open `scan` IVEC job
    /// during error recovery or pre-scan reset of an orphaned previous job.
    async fn abort_scan_session(&mut self, scan_job_id: u32, in_mp2_mode: bool) {
        let recovery_timeout = std::time::Duration::from_millis(2500);
        if in_mp2_mode {
            let cancel = build_cancel_or_finish_scan();
            let _ = self.transport.write_scanner(&cancel).await;
            self.drain_scanner_optional(recovery_timeout).await;
        }
        let end_job = build_end_job("scan", scan_job_id);
        let _ = self.transport.write_scanner(end_job.as_bytes()).await;
        self.drain_scanner_optional(recovery_timeout).await;
    }

    /// Acquires raw flatbed scanner data conforming to the MP2 scanner binary protocol.
    pub async fn acquire_raw_scan(
        &mut self,
        params: &ScanParameters,
    ) -> Result<Vec<u8>, DriverError> {
        params
            .geometry
            .validate(params.resolution)
            .map_err(DriverError::HardwareConstraint)?;

        // If 1200 DPI is requested, translate to native 600 DPI hardware scan
        // (Canon PIXMA G2010 CIS flatbed optical resolution is natively 600 DPI horizontal).
        // The acquired 600 DPI buffer is then 2x interpolated up to 1200 DPI.
        let is_1200 = params.resolution == pixma_core::scan_params::ScanResolution::Dpi1200;
        let hw_params = if is_1200 {
            let mut p = params.clone();
            p.resolution = pixma_core::scan_params::ScanResolution::Dpi600;
            p.geometry.x_offset /= 2;
            p.geometry.y_offset /= 2;
            p.geometry.width /= 2;
            p.geometry.height /= 2;
            p
        } else {
            params.clone()
        };

        hw_params
            .geometry
            .validate(hw_params.resolution)
            .map_err(DriverError::HardwareConstraint)?;

        let prev_scan_job_id = self.last_scan_job_id.unwrap_or(1);
        let scan_job_id = self.next_job_id();
        let mut rx_buf = [0u8; 1024];

        // 1. IVEC StartJob (with bounded recovery if a previous scan aborted in MP2 binary mode)
        tracing::info!("acquire_raw_scan: Step 1 StartJob (job_id={scan_job_id})");
        let start_job = build_start_job("scan", scan_job_id);
        self.transport.write_scanner(start_job.as_bytes()).await?;
        let probe_res = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            self.transport.read_scanner(&mut rx_buf),
        )
        .await;
        let initial_ok = match &probe_res {
            Ok(Ok(n)) if *n > 0 => Self::verify_ivec_xml_ok("StartJob", &rx_buf[..*n]),
            Ok(Ok(0)) => Err(DriverError::Protocol("StartJob returned empty reply".into())),
            Ok(Err(e)) => Err(DriverError::Transport(e.to_string())),
            Err(_) => Err(DriverError::Transport("StartJob timed out".into())),
            _ => unreachable!(),
        };

        match initial_ok {
            Ok(()) => {
                tracing::info!("acquire_raw_scan: StartJob OK");
            }
            Err(reason) => {
                tracing::warn!(
                    "acquire_raw_scan: StartJob failed ({reason}); attempting cancel recovery (prev_job_id={prev_scan_job_id})"
                );
                self.abort_scan_session(prev_scan_job_id, true).await;
                self.transport.write_scanner(start_job.as_bytes()).await?;
                let recovery_res = async {
                    let n = self.transport.read_scanner(&mut rx_buf).await?;
                    if n == 0 {
                        return Err(DriverError::Protocol(
                            "Recovered StartJob returned empty reply".into(),
                        ));
                    }
                    Self::verify_ivec_xml_ok("StartJob", &rx_buf[..n])?;
                    Ok(n)
                }
                .await;
                match recovery_res {
                    Ok(n) => {
                        tracing::info!("acquire_raw_scan: Recovered StartJob, read {n} bytes");
                    }
                    Err(err) => {
                        self.abort_scan_session(scan_job_id, false).await;
                        return Err(err);
                    }
                }
            }
        }

        // Only update last_scan_job_id once StartJob has actually been accepted.
        self.last_scan_job_id = Some(scan_job_id);

        let mut in_mp2_mode = false;
        let res = self
            .acquire_raw_scan_after_start_job(&hw_params, scan_job_id, is_1200, &mut in_mp2_mode)
            .await;
        if let Err(err) = res {
            self.abort_scan_session(scan_job_id, in_mp2_mode).await;
            return Err(err);
        }
        res
    }

    async fn acquire_raw_scan_after_start_job(
        &mut self,
        hw_params: &ScanParameters,
        scan_job_id: u32,
        is_1200: bool,
        in_mp2_mode: &mut bool,
    ) -> Result<Vec<u8>, DriverError> {
        let mut rx_buf = [0u8; 1024];

        // 2. ModeShift to binary MP2 mode
        tracing::info!("acquire_raw_scan: Step 2 ModeShift");
        let mode_shift = build_mode_shift("scan", scan_job_id, 1);
        self.transport.write_scanner(mode_shift.as_bytes()).await?;
        let n = match self.transport.read_scanner(&mut rx_buf).await {
            Ok(n) => n,
            Err(e) => {
                // ModeShift command was already written; conservatively exit MP2 during cleanup.
                *in_mp2_mode = true;
                return Err(e);
            }
        };
        tracing::info!("acquire_raw_scan: ModeShift returned {n} bytes");
        if n == 0 {
            return Err(DriverError::Protocol(
                "ModeShift returned empty reply".into(),
            ));
        }
        Self::verify_ivec_xml_ok("ModeShift", &rx_buf[..n])?;
        *in_mp2_mode = true;
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        // 3. StartSession (0xDB20), recovering if a previous session was left open or scanner is settling
        tracing::info!("acquire_raw_scan: Step 3 StartSession");
        let cmd = build_start_session();
        let mut n = 0usize;
        let max_session_attempts = 5;
        for attempt in 1..=max_session_attempts {
            self.transport.write_scanner(&cmd).await?;
            n = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                self.transport.read_scanner(&mut rx_buf),
            )
            .await
            .map_err(|_| {
                self.transport.clear_scanner_rx_overflow();
                DriverError::Transport("Scanner StartSession read timed out".into())
            })??;
            tracing::info!(
                "acquire_raw_scan: StartSession attempt {attempt}/{max_session_attempts} returned {n} bytes: {:02X?}",
                &rx_buf[..n.min(16)]
            );
            if n >= 2 && rx_buf[0] == 0x06 && rx_buf[1] == 0x06 {
                break;
            }
            if n >= 2 && rx_buf[0] == 0x15 && rx_buf[1] == 0x15 && attempt < max_session_attempts {
                let cancel = build_cancel_or_finish_scan();
                let _ = self.transport.write_scanner(&cancel).await;
                self.drain_scanner_optional(std::time::Duration::from_millis(2500))
                    .await;
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                continue;
            }
            return Err(DriverError::Protocol(format!(
                "Scanner rejected StartSession (0xDB20): {:02X?}",
                &rx_buf[..n.min(8)]
            )));
        }
        if n < 2 || rx_buf[0] != 0x06 || rx_buf[1] != 0x06 {
            return Err(DriverError::Protocol(format!(
                "Scanner StartSession (0xDB20) did not ACK: {:02X?}",
                &rx_buf[..n.min(8)]
            )));
        }

        // 4. SendGammaTable (0xEE20, 2072 bytes) x 3 channels (R, G, B) using hardware sRGB 2.2 LUT
        tracing::info!("acquire_raw_scan: Step 4 GammaTable");
        let gamma_cmd = build_gamma_table(2.2);
        for ch in 0..3 {
            self.transport.write_scanner(&gamma_cmd).await?;
            let n = self.transport.read_scanner(&mut rx_buf).await?;
            if n < 2 || rx_buf[0] != 0x06 || rx_buf[1] != 0x06 {
                return Err(DriverError::Protocol(format!(
                    "Scanner rejected SendGammaTable (channel {ch}): {:02X?}",
                    &rx_buf[..n.min(8)]
                )));
            }
        }

        // 5. SetScanParameter (0xD820, 72 bytes)
        tracing::info!("acquire_raw_scan: Step 5 SetScanParameter");
        let cmd =
            build_set_scan_parameter(hw_params).map_err(|e| DriverError::Protocol(e.to_string()))?;
        self.transport.write_scanner(&cmd).await?;
        let n = self.transport.read_scanner(&mut rx_buf).await?;
        tracing::info!(
            "acquire_raw_scan: SetScanParameter returned {n} bytes: {:02X?}",
            &rx_buf[..n.min(16)]
        );
        if n < 2 || rx_buf[0] != 0x06 || rx_buf[1] != 0x06 {
            return Err(DriverError::Protocol(format!(
                "Scanner rejected SetScanParameter (0xD820): {:02X?}",
                &rx_buf[..n.min(8)]
            )));
        }

        // 6. StartScan (0xD920)
        tracing::info!("acquire_raw_scan: Step 6 StartScan");
        let cmd = build_start_scan();
        self.transport.write_scanner(&cmd).await?;
        let n = self.transport.read_scanner(&mut rx_buf).await?;
        tracing::info!(
            "acquire_raw_scan: StartScan returned {n} bytes: {:02X?}",
            &rx_buf[..n.min(16)]
        );
        if n < 2 || rx_buf[0] != 0x06 || rx_buf[1] != 0x06 {
            return Err(DriverError::Protocol(format!(
                "Scanner rejected StartScan (0xD920): {:02X?}",
                &rx_buf[..n.min(8)]
            )));
        }

        // 7. Poll GetCalibrationStatus (0xDA20) until (resp[8] & 0x03) != 0 (calibrated & ready)
        for iter in 0..60 {
            let cmd = build_get_calibration_status();
            self.transport.write_scanner(&cmd).await?;
            let n = self.transport.read_scanner(&mut rx_buf).await?;
            if n >= 9 && rx_buf[0] == 0x06 && rx_buf[1] == 0x06 && (rx_buf[8] & 0x03) != 0 {
                if iter > 0 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                }
                break;
            }
            let _ = self.transport.poll_interrupt().await;
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }

        // 8. Loop ReadScanChunk (0xD420)
        let raw_width = aligned_scan_width(hw_params) as usize;
        let target_width = hw_params.geometry.width as usize;
        let xs = (hw_params.geometry.x_offset & 31) as usize;
        let bpp = hw_params.color_mode.bytes_per_pixel();
        let height = hw_params.geometry.height as usize;
        let target_raw_bytes = raw_width * height * bpp;

        let mut accumulated_bytes = Vec::with_capacity(target_raw_bytes);
        let mut chunk_buf = vec![0u8; 262_160];
        let req_word: u32 = 0x0008_0008;
        let mut busy_retries = 0u32;

        loop {
            let cmd = build_read_scan_chunk_with_req(req_word);
            self.transport.write_scanner(&cmd).await?;
            let mut total_read = self.transport.read_scanner(&mut chunk_buf).await?;
            if total_read == 0 {
                break;
            }

            if total_read == 8 && chunk_buf[0] == 0x15 && chunk_buf[1] == 0x15 {
                busy_retries += 1;
                if busy_retries > 2000 {
                    return Err(DriverError::Protocol(
                        "Scanner returned 0x1515 (NAK/BUSY) during ReadScanChunk".to_string(),
                    ));
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                continue;
            }
            busy_retries = 0;

            if total_read >= 16 && chunk_buf[0] == 0x06 && chunk_buf[1] == 0x06 {
                let payload_len =
                    u32::from_be_bytes(chunk_buf[12..16].try_into().unwrap()) as usize;
                if payload_len > MAX_SCAN_CHUNK_BYTES {
                    return Err(DriverError::Protocol(format!(
                        "MP2 scan chunk payload length ({payload_len} bytes) exceeds maximum allowed ({MAX_SCAN_CHUNK_BYTES} bytes)"
                    )));
                }
                let expected_total = 16usize.checked_add(payload_len).ok_or_else(|| {
                    DriverError::Protocol("MP2 scan chunk size overflow".into())
                })?;
                if chunk_buf.len() < expected_total {
                    chunk_buf.resize(expected_total, 0);
                }
                while total_read < expected_total {
                    let more = self
                        .transport
                        .read_scanner(&mut chunk_buf[total_read..expected_total])
                        .await?;
                    if more == 0 {
                        return Err(DriverError::Protocol(format!(
                            "Unexpected EOF while reading MP2 scan chunk: got {total_read} of {expected_total} bytes"
                        )));
                    }
                    total_read += more;
                }
            }

            let packet = parse_scan_chunk_response(&chunk_buf[..total_read])
                .map_err(|e| DriverError::Protocol(e.to_string()))?;

            if !packet.payload.is_empty() {
                accumulated_bytes.extend_from_slice(&packet.payload);
            }

            if packet.is_scan_complete
                || accumulated_bytes.len() >= target_raw_bytes
                || (packet.is_page_end && packet.payload.is_empty())
            {
                break;
            }

            if packet.payload.is_empty() {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        }

        // 9. Verify complete image frame & crop 32-pixel aligned hardware scanline width
        let raw_stride = raw_width * bpp;
        let target_stride = target_width * bpp;
        let expected_raw_bytes = raw_stride * height;
        if accumulated_bytes.len() < expected_raw_bytes {
            return Err(DriverError::Protocol(format!(
                "Incomplete MP2 scan image data: got {} of {expected_raw_bytes} bytes",
                accumulated_bytes.len()
            )));
        }

        if raw_width > target_width || xs > 0 {
            let xs_bytes = xs * bpp;
            if xs_bytes + target_stride > raw_stride {
                return Err(DriverError::Protocol(format!(
                    "Invalid MP2 crop geometry: xs_bytes={xs_bytes}, target_stride={target_stride}, raw_stride={raw_stride}"
                )));
            }
            let mut cropped = Vec::with_capacity(target_stride * height);
            for y in 0..height {
                let row_start = y * raw_stride + xs_bytes;
                cropped.extend_from_slice(
                    &accumulated_bytes[row_start..row_start + target_stride],
                );
            }
            accumulated_bytes = cropped;
        } else {
            accumulated_bytes.truncate(target_stride * height);
        }

        // 10. CancelOrFinishScan (0xEF20) & 11. IVEC EndJob (use full read_scanner timeout on normal completion)
        let cmd = build_cancel_or_finish_scan();
        let _ = self.transport.write_scanner(&cmd).await;
        let _ = self.transport.read_scanner(&mut rx_buf).await;

        let end_job = build_end_job("scan", scan_job_id);
        let _ = self.transport.write_scanner(end_job.as_bytes()).await;
        let _ = self.transport.read_scanner(&mut rx_buf).await;

        // 12. If 1200 DPI was requested, perform 2x bilinear upscale from 600 DPI hardware scan
        if is_1200 {
            let expected_hw_bytes = target_stride * height;
            if accumulated_bytes.len() < expected_hw_bytes {
                return Err(DriverError::Protocol(format!(
                    "Cannot upscale 1200 DPI scan: got {} of {expected_hw_bytes} bytes",
                    accumulated_bytes.len()
                )));
            }
            accumulated_bytes = upscale_2x_bilinear(
                &accumulated_bytes,
                hw_params.geometry.width as usize,
                hw_params.geometry.height as usize,
                bpp,
            );
        }

        Ok(accumulated_bytes)
    }
}

/// Upscales an interleaved raw pixel buffer (1-bpp grayscale or 3-bpp RGB)
/// by exactly 2x in width and height using bilinear interpolation.
pub fn upscale_2x_bilinear(src: &[u8], src_w: usize, src_h: usize, bpp: usize) -> Vec<u8> {
    let dst_w = src_w * 2;
    let dst_h = src_h * 2;
    let mut dst = vec![0u8; dst_w * dst_h * bpp];
    let src_stride = src_w * bpp;
    let dst_stride = dst_w * bpp;

    for y in 0..src_h {
        let y_next = (y + 1).min(src_h.saturating_sub(1));
        let src_row0 = &src[y * src_stride..(y + 1) * src_stride];
        let src_row1 = &src[y_next * src_stride..(y_next + 1) * src_stride];
        let dst_row0_idx = (y * 2) * dst_stride;
        let dst_row1_idx = (y * 2 + 1) * dst_stride;

        for x in 0..src_w {
            let x_next = (x + 1).min(src_w.saturating_sub(1));
            let p00 = x * bpp;
            let p10 = x_next * bpp;

            let out_x0 = (x * 2) * bpp;
            let out_x1 = (x * 2 + 1) * bpp;

            for c in 0..bpp {
                let v00 = src_row0[p00 + c] as u16;
                let v10 = src_row0[p10 + c] as u16;
                let v01 = src_row1[p00 + c] as u16;
                let v11 = src_row1[p10 + c] as u16;

                // (2x, 2y)
                dst[dst_row0_idx + out_x0 + c] = v00 as u8;
                // (2x+1, 2y)
                dst[dst_row0_idx + out_x1 + c] = (v00 + v10).div_ceil(2) as u8;
                // (2x, 2y+1)
                dst[dst_row1_idx + out_x0 + c] = (v00 + v01).div_ceil(2) as u8;
                // (2x+1, 2y+1)
                dst[dst_row1_idx + out_x1 + c] = ((v00 + v10 + v01 + v11 + 2) / 4) as u8;
            }
        }
    }
    dst
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::maintenance::{CleaningType, MaintenanceRecord, MaintenanceSupervisor};
    use pixma_core::models::{OperationalState, PrinterModel};
    use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
    use std::collections::VecDeque;

    struct ScriptedTransport {
        is_mock_flag: bool,
        serial: &'static str,
        printer_writes: Vec<Vec<u8>>,
        printer_rx: VecDeque<Vec<u8>>,
        scanner_writes: Vec<Vec<u8>>,
        scanner_rx: VecDeque<Vec<u8>>,
    }

    impl ScriptedTransport {
        fn new(is_mock_flag: bool, serial: &'static str) -> Self {
            Self {
                is_mock_flag,
                serial,
                printer_writes: Vec::new(),
                printer_rx: VecDeque::new(),
                scanner_writes: Vec::new(),
                scanner_rx: VecDeque::new(),
            }
        }
    }

    impl UsbTransport for ScriptedTransport {
        fn model(&self) -> PrinterModel {
            PrinterModel::G2010
        }

        fn serial_number(&self) -> &str {
            self.serial
        }

        fn is_mock(&self) -> bool {
            self.is_mock_flag
        }

        async fn get_device_id(&mut self) -> Result<String, DriverError> {
            Ok(crate::G2010_IEEE1284_DEVICE_ID.to_string())
        }

        async fn get_port_status(&mut self) -> Result<u8, DriverError> {
            Ok(0x18)
        }

        async fn soft_reset(&mut self) -> Result<(), DriverError> {
            Ok(())
        }

        async fn write_printer<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
            self.printer_writes.push(data.to_vec());
            Ok(data.len())
        }

        async fn read_printer<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
            let Some(chunk) = self.printer_rx.pop_front() else {
                return Ok(0);
            };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            Ok(n)
        }

        async fn write_scanner<'a>(&'a mut self, data: &'a [u8]) -> Result<usize, DriverError> {
            self.scanner_writes.push(data.to_vec());
            Ok(data.len())
        }

        async fn read_scanner<'a>(&'a mut self, buf: &'a mut [u8]) -> Result<usize, DriverError> {
            let Some(chunk) = self.scanner_rx.pop_front() else {
                return Ok(0);
            };
            let n = chunk.len().min(buf.len());
            buf[..n].copy_from_slice(&chunk[..n]);
            if chunk.len() > n {
                self.scanner_rx.push_front(chunk[n..].to_vec());
            }
            Ok(n)
        }

        async fn poll_interrupt(&mut self) -> Result<Option<u8>, DriverError> {
            Ok(None)
        }
    }

    fn status_xml(status: &str, detail: &str, support_code: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/" xmlns:vcn="http://www.canon.com/ns/cmd/2008/07/canon/">
  <ivec:contents>
    <ivec:operation>GetStatusResponse</ivec:operation>
    <ivec:response>OK</ivec:response>
    <ivec:status>{status}</ivec:status>
    <vcn:statusdetail>{detail}</vcn:statusdetail>
    <vcn:supportcode>{support_code}</vcn:supportcode>
    <vcn:serialnumber>UNKNOWN</vcn:serialnumber>
    <vcn:model>G2010 series</vcn:model>
  </ivec:contents>
</cmd>"#
        )
        .into_bytes()
    }

    fn queue_valid_scan_handshake(rx: &mut VecDeque<Vec<u8>>) {
        let ok_xml = br#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:response>OK</ivec:response></ivec:contents></cmd>"#.to_vec();
        let ack8 = vec![0x06, 0x06, 0, 0, 0, 0, 0, 0];
        let mut cal_ready = vec![0u8; 16];
        cal_ready[0] = 0x06;
        cal_ready[1] = 0x06;
        cal_ready[8] = 0x03;

        // 1: StartJob XML, 2: ModeShift XML, 3: StartSession, 4..6: 3x GammaTable,
        // 7: SetScanParameter, 8: StartScan, 9: GetCalibrationStatus
        rx.push_back(ok_xml.clone());
        rx.push_back(ok_xml);
        rx.push_back(ack8.clone());
        rx.push_back(ack8.clone());
        rx.push_back(ack8.clone());
        rx.push_back(ack8.clone());
        rx.push_back(ack8.clone());
        rx.push_back(ack8);
        rx.push_back(cal_ready);
    }

    fn test_scan_params() -> ScanParameters {
        ScanParameters {
            resolution: ScanResolution::Dpi300,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry {
                x_offset: 0,
                y_offset: 0,
                width: 64,
                height: 4,
            },
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn print_raw_stream_uses_is_mock_not_unknown_serial_string() {
        let mut transport = ScriptedTransport::new(false, "UNKNOWN");
        transport
            .printer_rx
            .push_back(status_xml("operator_call", "paperout", "1000"));
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let err = controller
            .print_raw_stream(b"dummy-stream")
            .await
            .expect_err("Non-mock transport with UNKNOWN serial must query hardware status");
        assert!(err.to_string().contains("Paper Out"), "unexpected err: {err}");
    }

    #[tokio::test]
    async fn query_ivec_with_drained_skips_stale_start_job_response() {
        let mut transport = ScriptedTransport::new(false, "SER123456");
        let stale_start_job = br#"<?xml version="1.0" encoding="utf-8" ?>
<cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/">
  <ivec:contents>
    <ivec:operation>StartJobResponse</ivec:operation>
    <ivec:response>OK</ivec:response>
  </ivec:contents>
</cmd>"#
            .to_vec();
        transport.printer_rx.push_back(stale_start_job);
        transport
            .printer_rx
            .push_back(status_xml("idle", "idle", "0000"));

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let (report, drained) = controller
            .query_status_with_drained()
            .await
            .expect("Should drain StartJobResponse and return GetStatusResponse");
        assert_eq!(report.status.state, OperationalState::Idle);
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].ivec_operation.as_deref(), Some("StartJobResponse"));
    }

    #[tokio::test]
    async fn acquire_raw_scan_rejects_oversized_mp2_chunk_header() {
        let mut transport = ScriptedTransport::new(true, "MOCK");
        queue_valid_scan_handshake(&mut transport.scanner_rx);

        let mut bad_hdr = vec![0u8; 16];
        bad_hdr[0] = 0x06;
        bad_hdr[1] = 0x06;
        let too_large = (MAX_SCAN_CHUNK_BYTES as u32) + 1;
        bad_hdr[12..16].copy_from_slice(&too_large.to_be_bytes());
        transport.scanner_rx.push_back(bad_hdr);

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let err = controller
            .acquire_raw_scan(&test_scan_params())
            .await
            .expect_err("Oversized MP2 scan chunk must be rejected");
        assert!(
            err.to_string().contains("exceeds maximum allowed"),
            "unexpected err: {err}"
        );
    }

    #[tokio::test]
    async fn acquire_raw_scan_rejects_early_eof_mid_chunk() {
        let mut transport = ScriptedTransport::new(true, "MOCK");
        queue_valid_scan_handshake(&mut transport.scanner_rx);

        let mut short_chunk = vec![0u8; 32];
        short_chunk[0] = 0x06;
        short_chunk[1] = 0x06;
        // Declare 1024-byte payload, but only 16 payload bytes are available before EOF (0).
        short_chunk[12..16].copy_from_slice(&1024u32.to_be_bytes());
        transport.scanner_rx.push_back(short_chunk);

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let err = controller
            .acquire_raw_scan(&test_scan_params())
            .await
            .expect_err("Truncated MP2 chunk before EOF must error");
        assert!(
            err.to_string().contains("Unexpected EOF while reading MP2 scan chunk"),
            "unexpected err: {err}"
        );
    }

    #[tokio::test]
    async fn print_raw_stream_records_clean_and_enforces_cooldown() {
        let transport = ScriptedTransport::new(true, "MOCK");
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let clean_cmd = build_cleaning_cmd(
            42,
            CleaningType::Regular,
            pixma_core::maintenance::CleaningTarget::ColorOnly,
        );
        controller
            .print_raw_stream(clean_cmd.as_bytes())
            .await
            .expect("First raw IVEC regular clean should succeed");

        assert!(
            controller.supervisor().record().last_standard_clean_epoch > 0,
            "Raw IVEC clean must record timestamp in MaintenanceSupervisor"
        );
        assert!(
            (controller.supervisor().record().estimated_d_counter_percent
                - CleaningType::Regular.estimated_d_counter_delta())
            .abs()
                < 0.01
        );

        let err = controller
            .print_raw_stream(clean_cmd.as_bytes())
            .await
            .expect_err("Immediate repeat raw IVEC regular clean must hit cooldown");
        assert!(
            matches!(err, DriverError::SafetyInterlock(SafetyInterlockError::CooldownActive { .. })),
            "unexpected err: {err}"
        );
    }

    #[tokio::test]
    async fn print_raw_stream_rejects_unconfirmed_raw_ink_flush() {
        let transport = ScriptedTransport::new(true, "MOCK");
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);

        let choke_cmd = build_cleaning_cmd(
            43,
            CleaningType::InkFlush,
            pixma_core::maintenance::CleaningTarget::All,
        );
        let err = controller
            .print_raw_stream(choke_cmd.as_bytes())
            .await
            .expect_err("Unconfirmed raw IVEC choke must be rejected before USB write");
        assert!(
            matches!(
                err,
                DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired { .. })
            ),
            "unexpected err: {err}"
        );
        assert!(
            controller.transport().printer_writes.is_empty(),
            "Rejected raw choke must not write any bytes to USB"
        );
    }

    #[tokio::test]
    async fn print_raw_stream_rejects_obfuscated_raw_ink_flush() {
        let obfuscated_payloads: &[(&str, &[u8])] = &[
            (
                "commented_startjob_prefix",
                b"<!-- <cmd><ivec:operation>StartJob</ivec:operation> -->\
                  <cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd>",
            ),
            (
                "pi_split_and_char_refs",
                b"<cmd><ivec:operation>Clean<?pi >?>ing</ivec:operation>\
                  <ivec:type>&#99;hoke</ivec:type></cmd>",
            ),
            (
                "oversized_attr_before_choke",
                // Constructed dynamically below; static check uses doctype + entity + comment
                b"<!DOCTYPE cmd [ <!ENTITY x \"<!--\"> ]>\
                  <cmd><ivec:operation>Cleaning</ivec:operation><ivec:type>choke</ivec:type></cmd><!-- end -->",
            ),
            (
                "doctype_internal_pi_quote_and_utf8_fallback",
                b"<!DOCTYPE cmd [ <?pi \" ] ?> ]>\
                  <cmd><ivec:operation>Cleaning</ivec:operation>\
                  <ivec:type>choke</ivec:type>\
                  <ivec:job_description>\"&0123456789abcd\x80</ivec:job_description></cmd>",
            ),
        ];

        for (label, payload) in obfuscated_payloads {
            let transport = ScriptedTransport::new(true, "MOCK");
            let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
            let mut controller = G2010Controller::new(transport, supervisor);
            let err = controller
                .print_raw_stream(payload)
                .await
                .expect_err(&format!(
                    "Obfuscated raw IVEC choke ({label}) must be rejected before USB write"
                ));
            assert!(
                matches!(
                    err,
                    DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired { .. })
                ),
                "{label}: unexpected err: {err}"
            );
            assert!(
                controller.transport().printer_writes.is_empty(),
                "{label}: rejected raw choke must not write any bytes to USB"
            );
        }

        // Also test >512-byte open tag attribute end-to-end through print_raw_stream
        let large_attr = "a".repeat(600);
        let long_tag_cmd = format!(
            "<ivec:cmd xmlns:p=\"{large_attr}\"><ivec:operation xmlns:q=\"{large_attr}\">Cleaning</ivec:operation>\
             <ivec:type xmlns:r=\"{large_attr}\">choke</ivec:type></ivec:cmd>"
        );
        let transport = ScriptedTransport::new(true, "MOCK");
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);
        let err = controller
            .print_raw_stream(long_tag_cmd.as_bytes())
            .await
            .expect_err("Large-attribute raw IVEC choke must be rejected before USB write");
        assert!(matches!(
            err,
            DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired { .. })
        ));
        assert!(controller.transport().printer_writes.is_empty());
    }

    #[tokio::test]
    async fn acquire_raw_scan_recovers_start_session_nak_and_cleans_up_on_failure() {
        // Case A: Transient 0x1515 on StartSession recovers on retry and finishes scan.
        let mut transport = ScriptedTransport::new(true, "MOCK");
        let ok_xml = br#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:response>OK</ivec:response></ivec:contents></cmd>"#.to_vec();
        let ack8 = vec![0x06, 0x06, 0, 0, 0, 0, 0, 0];
        let nak8 = vec![0x15, 0x15, 0, 0, 0, 0, 0, 0];
        let mut cal_ready = vec![0u8; 16];
        cal_ready[0] = 0x06;
        cal_ready[1] = 0x06;
        cal_ready[8] = 0x03;

        // 1: StartJob XML, 2: ModeShift XML, 3a: StartSession -> NAK,
        // 3a-cancel ACK, 3b: StartSession -> ACK, 4..6: 3x Gamma, 7: SetScanParam, 8: StartScan, 9: Calib
        transport.scanner_rx.push_back(ok_xml.clone());
        transport.scanner_rx.push_back(ok_xml.clone());
        transport.scanner_rx.push_back(nak8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(ack8.clone());
        transport.scanner_rx.push_back(cal_ready);

        // Minimal 1-chunk completed page (64x4 RGB = 768 bytes)
        let mut chunk = vec![0u8; 16 + 768];
        chunk[0] = 0x06;
        chunk[1] = 0x06;
        chunk[2] = 0x28;
        chunk[12..16].copy_from_slice(&(768u32).to_be_bytes());
        transport.scanner_rx.push_back(chunk);
        transport.scanner_rx.push_back(ack8);
        transport.scanner_rx.push_back(ok_xml.clone());

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);
        let raw = controller
            .acquire_raw_scan(&test_scan_params())
            .await
            .expect("Transient StartSession 0x1515 should recover on retry");
        assert_eq!(raw.len(), 768);

        // Case B: Persistent 0x1515 fails cleanly AND closes MP2 + EndJob so next scan isn't stuck.
        let mut fail_transport = ScriptedTransport::new(true, "MOCK");
        fail_transport.scanner_rx.push_back(ok_xml.clone());
        fail_transport.scanner_rx.push_back(ok_xml);
        for _ in 0..5 {
            fail_transport.scanner_rx.push_back(nak8.clone()); // StartSession -> 0x1515
            fail_transport.scanner_rx.push_back(vec![0x06, 0x06, 0, 0, 0, 0, 0, 0]); // Cancel reply
        }

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(fail_transport, supervisor);
        let err = controller
            .acquire_raw_scan(&test_scan_params())
            .await
            .expect_err("Repeated StartSession 0x1515 must fail cleanly");
        assert!(err.to_string().contains("0xDB20"), "unexpected err: {err}");

        let writes = &controller.transport().scanner_writes;
        assert!(
            writes
                .iter()
                .any(|w| w.starts_with(&[0xEF, 0x20])),
            "Failed StartSession must emit CancelOrFinishScan (0xEF20)"
        );
        assert!(
            writes
                .iter()
                .any(|w| std::str::from_utf8(w).is_ok_and(|s| s.contains("<ivec:operation>EndJob</ivec:operation>"))),
            "Failed StartSession must emit IVEC EndJob to avoid leaving scanner orphaned in MP2 mode"
        );
    }

    #[tokio::test]
    async fn acquire_raw_scan_validates_startjob_modeshift_xml_and_guards_1200dpi_eof() {
        let ok_xml = br#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:response>OK</ivec:response></ivec:contents></cmd>"#.to_vec();
        let busy_xml = br#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:response>Busy</ivec:response><ivec:response_detail>JobActive</ivec:response_detail></ivec:contents></cmd>"#.to_vec();
        let ng_xml = br#"<?xml version="1.0" encoding="utf-8" ?><cmd xmlns:ivec="http://www.canon.com/ns/cmd/2008/07/common/"><ivec:contents><ivec:response>NG</ivec:response><ivec:response_detail>InvalidMode</ivec:response_detail></ivec:contents></cmd>"#.to_vec();

        // Case 1: StartJob returns Busy XML -> runs cancel+EndJob recovery, then retries StartJob cleanly;
        // if ModeShift returns NG XML -> fails cleanly WITHOUT sending 0xEF20, and still closes job via EndJob.
        let mut transport = ScriptedTransport::new(true, "MOCK");
        transport.scanner_rx.push_back(busy_xml); // initial StartJob -> Busy
        transport.scanner_rx.push_back(vec![0x06, 0x06, 0, 0, 0, 0, 0, 0]); // recovery cancel reply
        transport.scanner_rx.push_back(ok_xml.clone()); // recovery EndJob(prev=1) reply
        transport.scanner_rx.push_back(ok_xml.clone()); // recovered StartJob -> OK
        transport.scanner_rx.push_back(ng_xml); // ModeShift -> NG

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(transport, supervisor);
        let err = controller
            .acquire_raw_scan(&test_scan_params())
            .await
            .expect_err("ModeShift NG XML must be rejected");
        assert!(err.to_string().contains("ModeShift rejected"), "unexpected err: {err}");
        assert_eq!(controller.last_scan_job_id, Some(1));

        // Verify the final abort after ModeShift NG did NOT send a second 0xEF20 after EndJob of job 1
        let writes = &controller.transport().scanner_writes;
        let cancel_count = writes.iter().filter(|w| w.starts_with(&[0xEF, 0x20])).count();
        assert_eq!(cancel_count, 1, "Only pre-StartJob recovery should emit 0xEF20 when ModeShift returned NG");

        // Case 2: 1200 DPI early EOF on ReadScanChunk (0 bytes) returns Protocol error instead of panicking in upscale_2x_bilinear
        let mut transport_1200 = ScriptedTransport::new(true, "MOCK");
        queue_valid_scan_handshake(&mut transport_1200.scanner_rx);
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut ctrl_1200 = G2010Controller::new(transport_1200, supervisor);
        let params_1200 = ScanParameters {
            resolution: ScanResolution::Dpi1200,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry {
                x_offset: 0,
                y_offset: 0,
                width: 128,
                height: 8,
            },
            ..Default::default()
        };
        let err = ctrl_1200
            .acquire_raw_scan(&params_1200)
            .await
            .expect_err("Truncated 1200 DPI scan must error cleanly without panic");
        assert!(err.to_string().contains("Incomplete MP2 scan image data"), "unexpected err: {err}");
    }
}
