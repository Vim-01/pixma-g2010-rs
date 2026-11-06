//! High-level hardware-safe controller for Canon PIXMA G-series devices.
//!
//! Orchestrates the complete print job lifecycle, thermal duty-cycle throttling,
//! maintenance cooldown authorization, and MP2 flatbed scan acquisition.

use crate::mock_device::MockG2010Transport;
use crate::nusb_transport::NusbTransport;
use crate::traits::UsbTransport;
use pixma_core::errors::{DriverError, SafetyInterlockError, SupportCodeInfo};
use pixma_core::maintenance::{MaintenanceAction, MaintenanceSupervisor};
use pixma_core::models::{DeviceStatus, PrinterModel};
use pixma_core::print_params::PrintJobConfig;
use pixma_core::safety::{PrintJobTypestate, ValidatedRasterLine};
use pixma_core::scan_params::ScanParameters;
use pixma_protocol::bjl::BjlBuilder;
use pixma_protocol::ivec::{
    build_bjl_set_time_send_data, build_cancel_job, build_cleaning_cmd, build_end_job,
    build_get_status_cmd, build_manual_alignment_values_cmd, build_mode_shift,
    build_roller_cleaning_cmd, build_send_data_header, build_set_job_configuration,
    build_set_page_configuration, build_start_job, build_test_print_cmd, parse_status_response,
};
use pixma_protocol::mp2_scanner::{
    build_cancel_or_finish_scan, build_get_calibration_status, build_get_data_status,
    build_read_scan_chunk, build_set_lamp, build_set_scan_parameter, build_start_scan,
    parse_scan_chunk_response,
};
use pixma_protocol::pwg_raster::{encode_pwg_page, PwgRasterHeader};

/// Unified transport enum enabling dynamic switching between physical USB (`nusb`)
/// and simulated virtual hardware (`MockG2010Transport`).
pub enum AnyTransport {
    Usb(NusbTransport),
    Mock(Box<MockG2010Transport>),
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
}

/// Hardware-safe controller managing transport I/O, maintenance interlocks, and print/scan lifecycles.
pub struct G2010Controller<T: UsbTransport> {
    transport: T,
    supervisor: MaintenanceSupervisor,
    job_counter: u32,
}

impl<T: UsbTransport> G2010Controller<T> {
    /// Creates a new controller wrapping the given USB transport and maintenance supervisor.
    pub fn new(transport: T, supervisor: MaintenanceSupervisor) -> Self {
        Self {
            transport,
            supervisor,
            job_counter: 1,
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

    /// Queries real-time hardware status and telemetry via IVEC XML (`GetStatus`).
    ///
    /// Updates the maintenance supervisor's estimated D-counter using reported values.
    pub async fn query_status(&mut self) -> Result<DeviceStatus, DriverError> {
        let cmd = build_get_status_cmd();
        self.transport.write_printer(cmd.as_bytes()).await?;

        let mut buf = vec![0u8; 4096];
        let bytes_read = self.transport.read_printer(&mut buf).await?;
        if bytes_read == 0 {
            return Err(DriverError::Transport(
                "Empty status reply from printer".into(),
            ));
        }

        let xml = String::from_utf8_lossy(&buf[..bytes_read]);
        tracing::debug!("Raw GetStatus XML ({bytes_read} bytes):\n{xml}");
        let mut status = parse_status_response(&xml)
            .map_err(|e| DriverError::Protocol(format!("Failed to parse status XML: {e}")))?;

        if status.serial_number == "UNKNOWN" && self.transport.serial_number() != "UNKNOWN" {
            status.serial_number = self.transport.serial_number().to_string();
        }
        status.model = self.transport.model();

        self.supervisor
            .update_d_counter_from_hardware(status.waste_absorber_percent);

        Ok(status)
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

        let _current_status = self.query_status().await?;
        let job_id = self.next_job_id();

        let mut rx_buf = [0u8; 1024];

        // Frame maintenance operation in protocol-2 job lifecycle:
        // SetJobConfiguration -> StartJob -> BJL SetTime -> Maintenance XML -> EndJob
        let set_job = build_set_job_configuration(job_id, None);
        self.transport.write_printer(set_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        let start_job = build_start_job("print", job_id);
        self.transport.write_printer(start_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        let bjl_send_data = build_bjl_set_time_send_data(job_id);
        self.transport
            .write_printer(bjl_send_data.as_bytes())
            .await?;
        let bjl_payload = BjlBuilder::time_sync_payload("20261107003000");
        self.transport.write_printer(&bjl_payload).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        match action {
            MaintenanceAction::Clean { kind, target } => {
                let cmd = build_cleaning_cmd(job_id, kind, target);
                self.transport.write_printer(cmd.as_bytes()).await?;
                let _ = self.transport.read_printer(&mut rx_buf).await;
                self.supervisor
                    .record_cleaning_executed(kind, now)
                    .map_err(DriverError::Io)?;
            }
            MaintenanceAction::MechanicalClean(target) => {
                let cmd = build_roller_cleaning_cmd(job_id, target);
                self.transport.write_printer(cmd.as_bytes()).await?;
                let _ = self.transport.read_printer(&mut rx_buf).await;
            }
            MaintenanceAction::TestPrint(test_type) => {
                let cmd = build_test_print_cmd(job_id, test_type);
                self.transport.write_printer(cmd.as_bytes()).await?;
                let _ = self.transport.read_printer(&mut rx_buf).await;
            }
            MaintenanceAction::ApplyManualAlignment { column_values } => {
                let cmd = build_manual_alignment_values_cmd(job_id, &column_values);
                self.transport.write_printer(cmd.as_bytes()).await?;
                let _ = self.transport.read_printer(&mut rx_buf).await;
            }
        }

        let end_job = build_end_job("print", job_id);
        self.transport.write_printer(end_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        self.query_status().await
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

        // 2. SetJobConfiguration
        let set_job = build_set_job_configuration(config.job_id, None);
        self.transport.write_printer(set_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        // 3. StartJob
        let start_job = build_start_job("print", config.job_id);
        self.transport.write_printer(start_job.as_bytes()).await?;
        let _ = self.transport.read_printer(&mut rx_buf).await;

        // 4. BJL Time Synchronization
        let bjl_send_data = build_bjl_set_time_send_data(config.job_id);
        self.transport
            .write_printer(bjl_send_data.as_bytes())
            .await?;
        let bjl_payload = BjlBuilder::time_sync_payload(timestamp);
        self.transport.write_printer(&bjl_payload).await?;
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
            let page_cfg_xml = build_set_page_configuration(config, page_no);
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

    /// Acquires raw flatbed scanner data conforming to the MP2 scanner binary protocol.
    pub async fn acquire_raw_scan(
        &mut self,
        params: &ScanParameters,
    ) -> Result<Vec<u8>, DriverError> {
        params
            .geometry
            .validate(params.resolution)
            .map_err(DriverError::HardwareConstraint)?;

        let scan_job_id = self.next_job_id();
        let mut rx_buf = [0u8; 1024];

        // 1. IVEC StartJob
        let start_job = build_start_job("scan", scan_job_id);
        self.transport.write_scanner(start_job.as_bytes()).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 2. ModeShift to binary MP2 mode
        let mode_shift = build_mode_shift("scan", scan_job_id, 1);
        self.transport.write_scanner(mode_shift.as_bytes()).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 3. GetDataStatus
        let cmd = build_get_data_status();
        self.transport.write_scanner(&cmd).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 4. GetCalibrationStatus
        let cmd = build_get_calibration_status();
        self.transport.write_scanner(&cmd).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 5. SetLamp (ON)
        let cmd = build_set_lamp(true);
        self.transport.write_scanner(&cmd).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 6. SetScanParameter (72 bytes)
        let cmd =
            build_set_scan_parameter(params).map_err(|e| DriverError::Protocol(e.to_string()))?;
        self.transport.write_scanner(&cmd).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 7. StartScan
        let cmd = build_start_scan();
        self.transport.write_scanner(&cmd).await?;
        let _ = self.transport.read_scanner(&mut rx_buf).await?;

        // 8. Loop ReadScanChunk
        let mut accumulated_bytes = Vec::new();
        let mut chunk_buf = vec![0u8; 65536];

        loop {
            let cmd = build_read_scan_chunk();
            self.transport.write_scanner(&cmd).await?;
            let n = self.transport.read_scanner(&mut chunk_buf).await?;
            if n == 0 {
                break;
            }

            let packet = parse_scan_chunk_response(&chunk_buf[..n])
                .map_err(|e| DriverError::Protocol(e.to_string()))?;

            accumulated_bytes.extend_from_slice(&packet.payload);

            if packet.is_scan_complete {
                break;
            }
        }

        // 9. SetLamp (OFF)
        let cmd = build_set_lamp(false);
        let _ = self.transport.write_scanner(&cmd).await;
        let _ = self.transport.read_scanner(&mut rx_buf).await;

        // 10. CancelOrFinishScan
        let cmd = build_cancel_or_finish_scan();
        let _ = self.transport.write_scanner(&cmd).await;
        let _ = self.transport.read_scanner(&mut rx_buf).await;

        // 11. IVEC EndJob
        let end_job = build_end_job("scan", scan_job_id);
        let _ = self.transport.write_scanner(end_job.as_bytes()).await;
        let _ = self.transport.read_scanner(&mut rx_buf).await;

        Ok(accumulated_bytes)
    }
}
