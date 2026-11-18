//! Pure-Rust, headlessly testable state and view-model for PIXMA G2010 Studio.

use image::RgbImage;
use pixma_core::errors::{SafetyInterlockError, SupportCodeInfo};
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, MaintenanceCapabilities, MaintenanceRecord,
    MaintenanceSupervisor, RollerCleanTarget, TestPrintType, INK_FLUSH_CONFIRMATION_TOKEN,
    MAX_SAFE_D_COUNTER_PERCENT,
};
use pixma_core::models::{DeviceStatus, OperationalState};
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintJobConfig, PrintQuality, QuietMode,
};
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_daemon::{
    local_hostname, ConnectionSource, DaemonClient, LiveDaemonSnapshot,
};
use pixma_imageproc::{decode_raw_scan_buffer, execute_scan_pipeline};
use pixma_raster::{generate_test_print_page, PrintRasterPipeline};
use serde::{Deserialize, Serialize};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 4-color hardware connection indicator strictly backed by real printer responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConnectionLedState {
    /// Red: no connection, USB/socket error, or printer powered off/offline.
    Disconnected,
    /// Yellow: printer is currently busy (active print/scan/maintenance lock or firmware busy state).
    Busy,
    /// Green: connected directly on local USB bus and ready.
    LocalUsbReady,
    /// Blue: connected remotely (e.g. via SSH socket bridge) and ready.
    RemoteReady,
}

impl ConnectionLedState {
    /// RGB color values in 0.0..=1.0 for Cairo LED rendering.
    pub fn rgb(self) -> (f64, f64, f64) {
        match self {
            Self::Disconnected => (0.88, 0.11, 0.14),
            Self::LocalUsbReady => (0.18, 0.76, 0.49),
            Self::Busy => (0.96, 0.83, 0.18),
            Self::RemoteReady => (0.21, 0.52, 0.89),
        }
    }

    /// Short Russian status label shown next to the LED indicator.
    pub fn short_label_ru(self) -> &'static str {
        match self {
            Self::Disconnected => "Нет подключения",
            Self::LocalUsbReady => "USB (локально)",
            Self::Busy => "Печатает / Занят",
            Self::RemoteReady => "Удалённо вкл",
        }
    }
}

/// Real-time connection channel state + latest uninterpolated printer telemetry snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectionTelemetry {
    pub connected: bool,
    pub local_hostname: String,
    pub source: Option<ConnectionSource>,
    pub snapshot: Option<LiveDaemonSnapshot>,
    pub last_error: Option<String>,
    pub configured_socket: Option<String>,
    pub configured_remote_host: Option<String>,
}

impl Default for ConnectionTelemetry {
    fn default() -> Self {
        Self {
            connected: false,
            local_hostname: local_hostname(),
            source: None,
            snapshot: None,
            last_error: None,
            configured_socket: None,
            configured_remote_host: None,
        }
    }
}

impl ConnectionTelemetry {
    /// Returns true if the active printer USB bus lives on another machine.
    pub fn is_remote(&self) -> bool {
        let Some(ref snap) = self.snapshot else {
            return matches!(self.source, Some(ConnectionSource::DaemonSocket { .. }))
                && self.configured_remote_host.is_some();
        };

        if matches!(self.source, Some(ConnectionSource::DirectUsb | ConnectionSource::DirectMock)) {
            return false;
        }

        if snap.hardware_marker.is_mock {
            return false;
        }

        let daemon_host = snap.daemon_hostname.trim();
        let client_host = self.local_hostname.trim();
        if !daemon_host.is_empty() && !client_host.is_empty() && daemon_host != client_host {
            return true;
        }

        matches!(self.source, Some(ConnectionSource::DaemonSocket { .. }))
            && self.configured_remote_host.is_some()
    }

    /// Computes the 4-color LED state exclusively from live telemetry and active lock state.
    pub fn led_state(&self, local_ui_busy: bool) -> ConnectionLedState {
        if !self.connected {
            return ConnectionLedState::Disconnected;
        }
        let Some(ref snap) = self.snapshot else {
            return ConnectionLedState::Disconnected;
        };

        if snap.status.state == OperationalState::Offline {
            return ConnectionLedState::Disconnected;
        }

        let hardware_busy = snap.active_job.is_some()
            || matches!(
                snap.status.state,
                OperationalState::Printing
                    | OperationalState::Scanning
                    | OperationalState::Cleaning
                    | OperationalState::Testing
                    | OperationalState::Waiting
            );

        if local_ui_busy || hardware_busy {
            return ConnectionLedState::Busy;
        }

        if self.is_remote() {
            ConnectionLedState::RemoteReady
        } else {
            ConnectionLedState::LocalUsbReady
        }
    }

    /// Formats a compact real USB marker string for the persistent header bar.
    pub fn usb_marker_short(&self) -> String {
        let Some(ref snap) = self.snapshot else {
            return if let Some(ref sock) = self.configured_socket {
                format!("Сокет: {sock} · нет ответа")
            } else {
                "USB: устройство не отвечает".to_string()
            };
        };

        let m = &snap.hardware_marker;
        if m.is_mock {
            return format!("МОК-ЭМУЛЯТОР · S/N {}", m.serial_number);
        }

        let bus_dev = match (m.bus_number, m.device_address) {
            (Some(b), Some(d)) => format!("USB Bus {b:03} Dev {d:03}"),
            _ => "USB".to_string(),
        };
        let vid_pid = match (m.vendor_id, m.product_id) {
            (Some(v), Some(p)) => format!("{v:04x}:{p:04x}"),
            _ => "????:????".to_string(),
        };
        format!("{bus_dev} · {vid_pid} · S/N {}", m.serial_number)
    }

    /// Decodes USB Printer Class `GET_PORT_STATUS` byte into real hardware signal bits.
    pub fn format_port_status(byte: u8) -> String {
        let paper = if byte & 0x20 != 0 {
            "Бумага: НЕТ (Paper Empty)"
        } else {
            "Бумага: есть"
        };
        let select = if byte & 0x10 != 0 {
            "Select: ON-LINE"
        } else {
            "Select: OFF-LINE"
        };
        let err = if byte & 0x08 != 0 {
            "Ошибка: нет"
        } else {
            "Ошибка: АППАРАТНАЯ"
        };
        format!("0x{byte:02X} ({paper} · {select} · {err})")
    }
}

/// Mandatory UI notice displayed when glossy media disables pigment black ink.
pub const GLOSSY_COMPOSITE_CMY_NOTICE: &str =
    "Composite CMY Mode Active (Pigment Black PGBK disabled to protect glossy coating)";

/// Complete view-model state for PIXMA G2010 Studio.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StudioViewModel {
    /// Print job configuration and layout parameters.
    pub print_config: PrintJobConfig,
    /// Flatbed scanner resolution, color mode, and post-processing filters.
    pub scan_params: ScanParameters,
    /// Live telemetry snapshot (ink levels, head state, D-counter, support codes).
    pub device_status: DeviceStatus,
    /// Real connection state, USB hardware marker, remote host vs local bus, and raw firmware reply.
    pub telemetry: ConnectionTelemetry,
    /// Persistent maintenance history and D-counter accounting.
    pub maintenance_record: MaintenanceRecord,
    /// Manual alignment column adjustment offsets for columns A..N (-5..=5).
    pub alignment_values: [i8; 14],
    /// Explicit user toggle state confirming system ink flush hazards.
    pub ink_flush_confirmed: bool,
    /// Notification banner text explaining composite CMY mode for glossy paper.
    pub glossy_notification: Option<String>,
    /// High-level diagnostic status banner message.
    pub status_banner: String,
    /// Last error message, if any.
    pub last_error: Option<String>,
    /// Busy indicator flag during active hardware operations.
    pub is_busy: bool,
    /// Active UI operation label while `is_busy` is true.
    pub busy_operation: Option<String>,
    /// Whether `--mock` / `PIXMA_MOCK=1` was explicitly requested.
    pub is_mock_mode: bool,
    /// Absolute filesystem path to the most recently exported scan image, if any.
    pub last_scan_path: Option<std::path::PathBuf>,
    /// Lightweight downscaled thumbnail path for non-blocking GTK preview.
    pub last_scan_thumb: Option<std::path::PathBuf>,
    /// Optional explicit output directory for saved scans (overrides `$XDG_PICTURES_DIR` / `~/Pictures/Scans`).
    #[serde(default)]
    pub scan_output_dir: Option<std::path::PathBuf>,
    /// Firmware maintenance capabilities (`GetCapability(maintenance)`) when available.
    #[serde(default)]
    pub maintenance_capabilities: Option<MaintenanceCapabilities>,
}

impl Default for StudioViewModel {
    fn default() -> Self {
        let print_config = PrintJobConfig::default();
        let scan_params = ScanParameters::default();
        let device_status = DeviceStatus::default();
        let maintenance_record = MaintenanceRecord::default();

        Self {
            print_config,
            scan_params,
            device_status,
            telemetry: ConnectionTelemetry::default(),
            maintenance_record,
            alignment_values: [0; 14],
            ink_flush_confirmed: false,
            glossy_notification: None,
            status_banner: String::from("Нет подключения: опрос устройства..."),
            last_error: None,
            is_busy: false,
            busy_operation: None,
            is_mock_mode: false,
            last_scan_path: None,
            last_scan_thumb: None,
            scan_output_dir: None,
            maintenance_capabilities: None,
        }
    }
}

/// Events that trigger state transitions in [`StudioViewModel`].
#[derive(Debug, Clone)]
pub enum StudioEvent {
    // Print preferences
    SetMediaType(MediaType),
    SetPaperSize(PaperSize),
    SetPrintQuality(PrintQuality),
    SetBorderless(BorderlessExtension),
    SetPageLayout(PageLayout),
    SetGrayscale(bool),
    SetCyanBalance(i8),
    SetMagentaBalance(i8),
    SetYellowBalance(i8),
    SetBrightness(i8),
    SetContrast(i8),
    SetPaperGap(PaperGapMode),
    SetDryingTime(InkDryingTime),
    SetQuietMode(QuietMode),

    // Scan preferences
    SetScanResolution(ScanResolution),
    SetScanColorMode(ScanColorMode),
    SetScanAutoCrop(bool),
    SetScanDeskew(bool),
    SetScanDescreen(bool),
    SetScanUnsharp(bool),
    SetScanRemoveDust(bool),
    SetScanGutterShadow(bool),
    SetScanGamma(f32),

    // Printhead alignment
    SetAlignmentColumn(usize, i8),
    SetAllAlignmentValues([i8; 14]),

    // Maintenance & safety
    SetInkFlushConfirmation(bool),
    ResetInkFlushConfirmation,
    SetMaintenanceCapabilities(Option<MaintenanceCapabilities>),

    // Status & telemetry
    UpdateDeviceStatus(DeviceStatus),
    ApplyLiveSnapshot {
        source: ConnectionSource,
        snapshot: Box<LiveDaemonSnapshot>,
    },
    ReportTelemetryDisconnected(String),
    UpdateMaintenanceRecord(MaintenanceRecord),
    SetStatusBanner(String),
    SetError(Option<String>),
    SetBusy(bool),
}

impl StudioViewModel {
    /// Constructs a new view-model in its default ready state.
    pub fn new() -> Self {
        Self::default()
    }

    /// Handles a synchronous view-model event and updates internal state.
    pub fn handle_event(&mut self, event: StudioEvent) {
        match event {
            StudioEvent::SetMediaType(media) => {
                self.print_config.media_type = media;
                if media.requires_composite_cmy_black() {
                    self.glossy_notification = Some(GLOSSY_COMPOSITE_CMY_NOTICE.to_string());
                    self.status_banner = GLOSSY_COMPOSITE_CMY_NOTICE.to_string();
                } else {
                    self.glossy_notification = None;
                    if self.status_banner == GLOSSY_COMPOSITE_CMY_NOTICE {
                        self.update_status_banner_from_device();
                    }
                }
            }
            StudioEvent::SetPaperSize(size) => {
                self.print_config.paper_size = size;
            }
            StudioEvent::SetPrintQuality(q) => {
                self.print_config.quality = q;
            }
            StudioEvent::SetBorderless(b) => {
                self.print_config.borderless = b;
            }
            StudioEvent::SetPageLayout(l) => {
                self.print_config.layout = l;
            }
            StudioEvent::SetGrayscale(gray) => {
                self.print_config.color.grayscale = gray;
            }
            StudioEvent::SetCyanBalance(val) => {
                self.print_config.color.cyan_balance = val.clamp(-50, 50);
            }
            StudioEvent::SetMagentaBalance(val) => {
                self.print_config.color.magenta_balance = val.clamp(-50, 50);
            }
            StudioEvent::SetYellowBalance(val) => {
                self.print_config.color.yellow_balance = val.clamp(-50, 50);
            }
            StudioEvent::SetBrightness(val) => {
                self.print_config.color.brightness = val.clamp(-50, 50);
            }
            StudioEvent::SetContrast(val) => {
                self.print_config.color.contrast = val.clamp(-50, 50);
            }
            StudioEvent::SetPaperGap(gap) => {
                self.print_config.paper_gap = gap;
            }
            StudioEvent::SetDryingTime(d) => {
                self.print_config.drying_time = d;
            }
            StudioEvent::SetQuietMode(q) => {
                self.print_config.quiet_mode = q;
            }

            // Scanner events
            StudioEvent::SetScanResolution(res) => {
                self.scan_params.resolution = res;
                self.scan_params.geometry = ScanGeometry::full_platen(res);
            }
            StudioEvent::SetScanColorMode(mode) => {
                self.scan_params.color_mode = mode;
            }
            StudioEvent::SetScanAutoCrop(val) => {
                self.scan_params.auto_multi_crop = val;
            }
            StudioEvent::SetScanDeskew(val) => {
                self.scan_params.deskew = val;
            }
            StudioEvent::SetScanDescreen(val) => {
                self.scan_params.descreen_moire = val;
            }
            StudioEvent::SetScanUnsharp(val) => {
                self.scan_params.unsharp_mask = val;
            }
            StudioEvent::SetScanRemoveDust(val) => {
                self.scan_params.remove_dust_scratches = val;
            }
            StudioEvent::SetScanGutterShadow(val) => {
                self.scan_params.correct_gutter_shadow = val;
            }
            StudioEvent::SetScanGamma(val) => {
                self.scan_params.gamma = val.clamp(0.1, 5.0);
            }

            // Alignment
            StudioEvent::SetAlignmentColumn(col_idx, val) => {
                if col_idx < 14 {
                    self.alignment_values[col_idx] = val.clamp(-5, 5);
                }
            }
            StudioEvent::SetAllAlignmentValues(vals) => {
                for (i, v) in vals.iter().enumerate().take(14) {
                    self.alignment_values[i] = (*v).clamp(-5, 5);
                }
            }

            // Maintenance
            StudioEvent::SetInkFlushConfirmation(confirmed) => {
                self.ink_flush_confirmed = confirmed;
            }
            StudioEvent::ResetInkFlushConfirmation => {
                self.ink_flush_confirmed = false;
            }
            StudioEvent::SetMaintenanceCapabilities(caps) => {
                self.maintenance_capabilities = caps;
            }

            // Status updates
            StudioEvent::UpdateDeviceStatus(status) => {
                self.device_status = status;
                self.telemetry.connected = true;
                self.update_status_banner_from_device();
            }
            StudioEvent::ApplyLiveSnapshot { source, snapshot } => {
                self.telemetry.connected = true;
                self.telemetry.source = Some(source);
                self.telemetry.last_error = None;
                self.device_status = snapshot.status.clone();
                self.telemetry.snapshot = Some(*snapshot);
                self.last_error = None;
                self.update_status_banner_from_device();
            }
            StudioEvent::ReportTelemetryDisconnected(err) => {
                self.telemetry.connected = false;
                self.telemetry.source = None;
                self.telemetry.snapshot = None;
                self.telemetry.last_error = Some(err.clone());
                self.last_error = Some(err.clone());
                self.maintenance_capabilities = None;
                self.status_banner = format!("Нет подключения: {err}");
            }
            StudioEvent::UpdateMaintenanceRecord(record) => {
                self.maintenance_record = record;
            }
            StudioEvent::SetStatusBanner(banner) => {
                self.status_banner = banner;
            }
            StudioEvent::SetError(err) => {
                self.last_error = err;
            }
            StudioEvent::SetBusy(busy) => {
                self.is_busy = busy;
                if !busy {
                    self.busy_operation = None;
                }
            }
        }
    }

    /// Formats a human-readable one-line print configuration summary.
    pub fn ui_print_summary(&self) -> String {
        let mut summary = format!(
            "{:?} | {:?} | Quality {:?}",
            self.print_config.media_type, self.print_config.paper_size, self.print_config.quality
        );
        if let Some(ref notice) = self.glossy_notification {
            summary.push_str(&format!(" — [!] {notice}"));
        }
        summary
    }

    /// Current 4-color LED connection state derived strictly from real hardware state.
    pub fn led_state(&self) -> ConnectionLedState {
        self.telemetry.led_state(self.is_busy)
    }

    /// Updates status banner based on connection, active support codes, or device operational state.
    fn update_status_banner_from_device(&mut self) {
        if !self.telemetry.connected {
            self.status_banner = match &self.telemetry.last_error {
                Some(err) => format!("Нет подключения: {err}"),
                None => String::from("Нет подключения: опрос устройства..."),
            };
            return;
        }

        if let Some(ref code) = self.device_status.support_code {
            if let Some(info) = SupportCodeInfo::lookup(code) {
                self.status_banner = format!(
                    "[ALERT] Код поддержки {} (LCD: {}): {}",
                    info.support_code, info.lcd_code, info.title
                );
            } else {
                self.status_banner = format!("[ALERT] Код поддержки: {code}");
            }
        } else if let Some(ref notice) = self.glossy_notification {
            self.status_banner = notice.clone();
        } else if let Some(ref snap) = self.telemetry.snapshot {
            let led = self.led_state();
            let scope = if self.telemetry.is_remote() {
                format!("Удалённо: {}", snap.daemon_hostname)
            } else {
                self.telemetry.usb_marker_short()
            };
            self.status_banner = format!(
                "{} ({:?}) · {}",
                led.short_label_ru(),
                snap.status.state,
                scope
            );
        } else {
            self.status_banner = format!("Подключено · Device Ready ({:?})", self.device_status.state);
        }
    }

    // -----------------------------------------------------------------------
    // Asynchronous hardware dispatcher methods
    // -----------------------------------------------------------------------

    /// Queries telemetry from the printer and updates local status.
    pub async fn dispatch_refresh_status(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<DeviceStatus, String> {
        self.is_busy = true;
        let res = client.get_status().await.map_err(|e| e.to_string());
        self.is_busy = false;

        match res {
            Ok(status) => {
                self.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
                self.last_error = None;
                Ok(status)
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Dispatches a print head cleaning routine after enforcing D-counter safety checks.
    pub async fn dispatch_cleaning(
        &mut self,
        client: &mut DaemonClient,
        kind: CleaningType,
        target: CleaningTarget,
        override_cooldown: bool,
    ) -> Result<DeviceStatus, String> {
        if kind == CleaningType::InkFlush {
            if self.device_status.waste_absorber_percent >= MAX_SAFE_D_COUNTER_PERCENT {
                let err = format!(
                    "{}",
                    SafetyInterlockError::AbsorberNearCapacity {
                        current_percent: self.device_status.waste_absorber_percent,
                        max_safe_percent: MAX_SAFE_D_COUNTER_PERCENT,
                    }
                );
                self.last_error = Some(err.clone());
                return Err(err);
            }
            if !self.ink_flush_confirmed {
                let err = format!(
                    "{}",
                    SafetyInterlockError::ConfirmationRequired {
                        expected_token: INK_FLUSH_CONFIRMATION_TOKEN,
                    }
                );
                self.last_error = Some(err.clone());
                return Err(err);
            }
        }

        if let Some(ref caps) = self.maintenance_capabilities {
            let kind_tok = kind.ivec_xml_value();
            let target_tok = target.ivec_xml_value();
            if caps.supports(kind_tok, target_tok) == Some(false) {
                let allowed = caps.format_summary().unwrap_or_else(|| "unknown".into());
                let err = format!(
                    "Firmware rejected {kind:?} for {target:?} (token '{target_tok}'): printer capability allows {allowed}."
                );
                self.ink_flush_confirmed = false;
                self.last_error = Some(err.clone());
                return Err(err);
            }
        }

        self.is_busy = true;
        let token = if kind == CleaningType::InkFlush {
            Some(INK_FLUSH_CONFIRMATION_TOKEN)
        } else {
            None
        };

        let action = MaintenanceAction::Clean { kind, target };
        let res = client
            .execute_maintenance(action, token, override_cooldown)
            .await
            .map_err(|e| e.to_string());

        self.is_busy = false;

        match res {
            Ok(status) => {
                self.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
                self.ink_flush_confirmed = false;
                self.last_error = None;
                Ok(status)
            }
            Err(e) => {
                self.ink_flush_confirmed = false;
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Dispatches feed roller or platen bottom plate cleaning.
    pub async fn dispatch_mechanical_clean(
        &mut self,
        client: &mut DaemonClient,
        target: RollerCleanTarget,
    ) -> Result<DeviceStatus, String> {
        self.is_busy = true;
        let action = MaintenanceAction::MechanicalClean(target);
        let res = client
            .execute_maintenance(action, None, false)
            .await
            .map_err(|e| e.to_string());
        self.is_busy = false;

        match res {
            Ok(status) => {
                self.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
                self.last_error = None;
                Ok(status)
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Dispatches a diagnostic test print (nozzle check, alignment sheet).
    pub async fn dispatch_test_print(
        &mut self,
        client: &mut DaemonClient,
        test_type: TestPrintType,
    ) -> Result<DeviceStatus, String> {
        self.is_busy = true;
        let action = MaintenanceAction::TestPrint(test_type);
        let res = client
            .execute_maintenance(action, None, false)
            .await
            .map_err(|e| e.to_string());
        self.is_busy = false;

        match res {
            Ok(status) => {
                self.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
                self.last_error = None;
                Ok(status)
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Dispatches manual printhead alignment column adjustment offsets A..N.
    pub async fn dispatch_apply_alignment(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<DeviceStatus, String> {
        self.is_busy = true;
        let action = MaintenanceAction::ApplyManualAlignment {
            column_values: self.alignment_values.to_vec(),
        };
        let res = client
            .execute_maintenance(action, None, false)
            .await
            .map_err(|e| e.to_string());
        self.is_busy = false;

        match res {
            Ok(status) => {
                self.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
                self.last_error = None;
                Ok(status)
            }
            Err(e) => {
                self.last_error = Some(e.clone());
                Err(e)
            }
        }
    }

    /// Acquires an image from the flatbed and runs the ScanGear Pro image pipeline.
    pub async fn dispatch_scan(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<Vec<RgbImage>, String> {
        self.is_busy = true;
        let res = self.dispatch_scan_inner(client).await;
        self.is_busy = false;
        match &res {
            Ok(_) => self.last_error = None,
            Err(err) => self.last_error = Some(err.clone()),
        }
        res
    }

    async fn dispatch_scan_inner(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<Vec<RgbImage>, String> {
        // Ensure scan geometry matches full platen resolution
        let full = ScanGeometry::full_platen(self.scan_params.resolution);

        let mut scan_params = self.scan_params.clone();
        scan_params.geometry = full;

        let raw_bytes = client
            .acquire_scan(&scan_params)
            .await
            .map_err(|e| e.to_string())?;

        let width = scan_params.geometry.width;
        let height = scan_params.geometry.height;

        let raw_img = decode_raw_scan_buffer(raw_bytes, width, height, scan_params.color_mode)
            .map_err(|e| e.to_string())?;

        let processed = execute_scan_pipeline(raw_img, &scan_params).map_err(|e| e.to_string())?;

        let scans_dir = resolve_scans_dir(self.scan_output_dir.as_deref())?;
        std::fs::create_dir_all(&scans_dir).map_err(|e| {
            format!(
                "Failed to create scan output directory {}: {e}",
                scans_dir.display()
            )
        })?;

        let timestamp = pixma_core::current_utc_timestamp_14();

        for (idx, img) in processed.iter().enumerate() {
            let filename = if processed.len() == 1 {
                format!("scan_{timestamp}.png")
            } else {
                format!("scan_{timestamp}_page{}.png", idx + 1)
            };
            let path = scans_dir.join(&filename);
            img.save(&path)
                .map_err(|e| format!("Failed to save scan image {}: {e}", path.display()))?;
            self.last_scan_path = Some(path);

            // Also save lightweight downscaled thumbnail for instant GTK preview
            let thumb_path = scans_dir.join(format!(".thumb_{filename}"));
            let thumb_w = 512u32;
            let thumb_h = ((img.height() as f32 / img.width() as f32) * thumb_w as f32).max(1.0) as u32;
            let thumb = image::imageops::resize(
                img,
                thumb_w,
                thumb_h,
                image::imageops::FilterType::Nearest,
            );
            thumb.save(&thumb_path).map_err(|e| {
                format!(
                    "Failed to save scan thumbnail {}: {e}",
                    thumb_path.display()
                )
            })?;
            self.last_scan_thumb = Some(thumb_path);
        }

        Ok(processed)
    }

    /// Renders and prints an internal diagnostic test page.
    pub async fn dispatch_print_test_page(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<(), String> {
        self.is_busy = true;
        let res = self.dispatch_print_test_page_inner(client).await;
        self.is_busy = false;
        match &res {
            Ok(()) => self.last_error = None,
            Err(err) => self.last_error = Some(err.clone()),
        }
        res
    }

    async fn dispatch_print_test_page_inner(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<(), String> {
        let dpi = self.print_config.quality.base_raster_dpi();
        let page = generate_test_print_page(dpi);
        let timestamp = pixma_core::current_utc_timestamp_14();

        let wire_data =
            PrintRasterPipeline::compile_ivec_bjraster3_job(&[page], &self.print_config, &timestamp)
                .map_err(|e| e.to_string())?;

        client
            .print_raw_stream(&wire_data)
            .await
            .map_err(|e| e.to_string())
    }
}

fn resolve_scans_dir(override_dir: Option<&Path>) -> Result<PathBuf, String> {
    let xdg_pictures = std::env::var_os("XDG_PICTURES_DIR");
    let home = std::env::var_os("HOME");
    resolve_scans_dir_from(override_dir, xdg_pictures.as_deref(), home.as_deref())
}

fn resolve_scans_dir_from(
    override_dir: Option<&Path>,
    xdg_pictures_dir: Option<&OsStr>,
    home: Option<&OsStr>,
) -> Result<PathBuf, String> {
    if let Some(dir) = override_dir {
        return Ok(dir.to_path_buf());
    }
    if let Some(xdg) = xdg_pictures_dir.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(xdg).join("Scans"));
    }
    if let Some(home_dir) = home.filter(|v| !v.is_empty()) {
        return Ok(PathBuf::from(home_dir).join("Pictures").join("Scans"));
    }
    Err(
        "Unable to determine scan output directory: neither XDG_PICTURES_DIR nor HOME is set"
            .to_string(),
    )
}

/// Headless self-test runner for CI and containerized environments.
pub async fn run_self_test() -> Result<(), String> {
    println!("[Self-Test] Initializing StudioViewModel...");
    let mut model = StudioViewModel::default();
    let selftest_dir = std::env::temp_dir().join(format!(
        "pixma-studio-selftest-{}-{}",
        pixma_daemon::current_uid(),
        std::process::id()
    ));
    model.scan_output_dir = Some(selftest_dir.clone());
    let res = run_self_test_inner(&mut model).await;
    let _ = std::fs::remove_dir_all(&selftest_dir);
    res
}

async fn run_self_test_inner(model: &mut StudioViewModel) -> Result<(), String> {
    use pixma_core::models::PrinterModel;
    use pixma_transport::controller::{AnyTransport, G2010Controller};
    use pixma_transport::mock_device::MockG2010Transport;

    assert_eq!(model.print_config.paper_size, PaperSize::A4);
    assert_eq!(model.print_config.media_type, MediaType::Plain);
    assert_eq!(model.glossy_notification, None);
    assert!(!model
        .ui_print_summary()
        .contains(GLOSSY_COMPOSITE_CMY_NOTICE));

    // 1. Glossy Media Notification check
    println!("[Self-Test] Testing glossy photo paper composite CMY warning...");
    model.handle_event(StudioEvent::SetMediaType(MediaType::GlossyPhoto));
    assert!(model.print_config.media_type.requires_composite_cmy_black());
    assert_eq!(
        model.glossy_notification.as_deref(),
        Some(GLOSSY_COMPOSITE_CMY_NOTICE)
    );
    assert!(model
        .ui_print_summary()
        .contains(GLOSSY_COMPOSITE_CMY_NOTICE));

    // Switch back to plain paper
    model.handle_event(StudioEvent::SetMediaType(MediaType::Plain));
    assert_eq!(model.glossy_notification, None);
    assert!(!model
        .ui_print_summary()
        .contains(GLOSSY_COMPOSITE_CMY_NOTICE));

    // 2. Alignment state check
    println!("[Self-Test] Testing alignment column state transitions...");
    model.handle_event(StudioEvent::SetAlignmentColumn(0, 3));
    assert_eq!(model.alignment_values[0], 3);
    model.handle_event(StudioEvent::SetAlignmentColumn(0, 7)); // Clamped to 5
    assert_eq!(model.alignment_values[0], 5);
    model.handle_event(StudioEvent::SetAlignmentColumn(1, -7)); // Clamped to -5
    assert_eq!(model.alignment_values[1], -5);

    // 3. Ink Flush Confirmation toggle
    println!("[Self-Test] Testing Ink Flush confirmation state...");
    model.handle_event(StudioEvent::SetInkFlushConfirmation(true));
    assert!(model.ink_flush_confirmed);
    model.handle_event(StudioEvent::ResetInkFlushConfirmation);
    assert!(!model.ink_flush_confirmed);

    // 4. Mock execution tests
    println!("[Self-Test] Testing headless execution with MockG2010Transport...");
    let mock = MockG2010Transport::new();
    let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
    let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let mut client = DaemonClient::direct(ctrl);

    // Refresh telemetry
    let status = model
        .dispatch_refresh_status(&mut client)
        .await
        .map_err(|e| format!("dispatch_refresh_status failed: {e}"))?;
    assert_eq!(status.model, PrinterModel::G2010);
    assert_eq!(status.ink_levels.pigment_black_percent, 100);

    // Flush blocked without confirmation
    let flush_res = model
        .dispatch_cleaning(
            &mut client,
            CleaningType::InkFlush,
            CleaningTarget::All,
            false,
        )
        .await;
    assert!(
        flush_res.is_err(),
        "Ink flush should be blocked when unconfirmed"
    );

    // Flush blocked if D-counter >= 80%
    model.device_status.waste_absorber_percent = 85.0;
    model.handle_event(StudioEvent::SetInkFlushConfirmation(true));
    let flush_blocked_absorber = model
        .dispatch_cleaning(
            &mut client,
            CleaningType::InkFlush,
            CleaningTarget::All,
            false,
        )
        .await;
    assert!(
        flush_blocked_absorber.is_err(),
        "Ink flush should be blocked when absorber >= 80%"
    );
    model.device_status.waste_absorber_percent = 0.0;

    // Regular clean
    model
        .dispatch_cleaning(
            &mut client,
            CleaningType::Regular,
            CleaningTarget::All,
            true,
        )
        .await
        .map_err(|e| format!("dispatch_cleaning failed: {e}"))?;

    // Nozzle check
    model
        .dispatch_test_print(&mut client, TestPrintType::NozzleCheck)
        .await
        .map_err(|e| format!("dispatch_test_print failed: {e}"))?;

    // Apply alignment
    model
        .dispatch_apply_alignment(&mut client)
        .await
        .map_err(|e| format!("dispatch_apply_alignment failed: {e}"))?;

    // Scan
    model.handle_event(StudioEvent::SetScanResolution(ScanResolution::Dpi300));
    model.handle_event(StudioEvent::SetScanColorMode(ScanColorMode::Color24Bit));
    let scan_pages = model
        .dispatch_scan(&mut client)
        .await
        .map_err(|e| format!("dispatch_scan failed: {e}"))?;
    assert!(!scan_pages.is_empty(), "Scan should produce pages");

    // Print test page
    model
        .dispatch_print_test_page(&mut client)
        .await
        .map_err(|e| format!("dispatch_print_test_page failed: {e}"))?;

    println!("[Self-Test] All StudioViewModel checks passed successfully!");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::models::{PrinterModel, UsbHardwareMarker};
    use pixma_daemon::JobLockKind;

    fn sample_snapshot(
        host: &str,
        state: OperationalState,
        active_job: Option<JobLockKind>,
    ) -> Box<LiveDaemonSnapshot> {
        let status = DeviceStatus {
            model: PrinterModel::G2010,
            serial_number: "1234AB".to_string(),
            state,
            ..Default::default()
        };
        Box::new(LiveDaemonSnapshot {
            daemon_hostname: host.to_string(),
            hardware_marker: UsbHardwareMarker {
                is_mock: false,
                bus_number: Some(1),
                device_address: Some(5),
                vendor_id: Some(0x04a9),
                product_id: Some(0x183a),
                serial_number: "1234AB".to_string(),
                manufacturer: Some("Canon".to_string()),
                product_name: Some("G2010 series".to_string()),
                endpoints_summary: "IF1 Print(OUT 0x01, IN 0x82)".to_string(),
            },
            status,
            ivec_response: Some("OK".to_string()),
            raw_ivec_status: Some("idle".to_string()),
            raw_status_detail: None,
            raw_support_code: None,
            firmware_reported_ink: false,
            firmware_reported_waste: false,
            firmware_reported_version: false,
            ieee1284_id: Some("MFG:Canon;MDL:G2010 series;".to_string()),
            usb_port_status: Some(0x18),
            active_job,
            telemetry_cached_during_job: active_job.is_some(),
            queried_at_unix_secs: 1_794_674_000,
        })
    }

    #[test]
    fn led_state_transitions_strictly_reflect_real_connection_and_busy_state() {
        let mut vm = StudioViewModel::default();
        vm.telemetry.local_hostname = "workstation-host".to_string();

        // 1. Disconnected by default -> Red
        assert_eq!(vm.led_state(), ConnectionLedState::Disconnected);

        // 2. Connected locally on USB, idle -> Green
        vm.handle_event(StudioEvent::ApplyLiveSnapshot {
            source: ConnectionSource::DirectUsb,
            snapshot: sample_snapshot("workstation-host", OperationalState::Idle, None),
        });
        assert_eq!(vm.led_state(), ConnectionLedState::LocalUsbReady);
        assert!(!vm.telemetry.is_remote());
        assert_eq!(
            vm.telemetry.usb_marker_short(),
            "USB Bus 001 Dev 005 · 04a9:183a · S/N 1234AB"
        );

        // 3. Connected remotely via SSH bridge (different daemon hostname), idle -> Blue
        vm.handle_event(StudioEvent::ApplyLiveSnapshot {
            source: ConnectionSource::DaemonSocket {
                socket_path: "/run/user/1000/pixma/remote.sock".into(),
            },
            snapshot: sample_snapshot("usb-server-host", OperationalState::Idle, None),
        });
        assert!(vm.telemetry.is_remote());
        assert_eq!(vm.led_state(), ConnectionLedState::RemoteReady);

        // 4. Remote active printing lock -> Yellow
        vm.handle_event(StudioEvent::ApplyLiveSnapshot {
            source: ConnectionSource::DaemonSocket {
                socket_path: "/run/user/1000/pixma/remote.sock".into(),
            },
            snapshot: sample_snapshot("usb-server-host", OperationalState::Idle, Some(JobLockKind::Printing)),
        });
        assert_eq!(vm.led_state(), ConnectionLedState::Busy);

        // 5. Local UI operation running while connected -> Yellow
        vm.handle_event(StudioEvent::ApplyLiveSnapshot {
            source: ConnectionSource::DirectUsb,
            snapshot: sample_snapshot("workstation-host", OperationalState::Idle, None),
        });
        vm.handle_event(StudioEvent::SetBusy(true));
        assert_eq!(vm.led_state(), ConnectionLedState::Busy);

        // 6. Disconnection clears stale snapshot -> Red even if local busy was cleared
        vm.handle_event(StudioEvent::SetBusy(false));
        vm.handle_event(StudioEvent::ReportTelemetryDisconnected(
            "early eof".to_string(),
        ));
        assert_eq!(vm.led_state(), ConnectionLedState::Disconnected);
        assert!(vm.telemetry.snapshot.is_none());
    }

    #[test]
    fn resolve_scans_dir_prefers_override_then_xdg_then_home() {
        assert_eq!(
            resolve_scans_dir_from(
                Some(Path::new("/var/tmp/custom-scans")),
                Some(OsStr::new("/home/u/Pics")),
                Some(OsStr::new("/home/u")),
            )
            .unwrap(),
            PathBuf::from("/var/tmp/custom-scans")
        );
        assert_eq!(
            resolve_scans_dir_from(
                None,
                Some(OsStr::new("/mnt/media/Pictures")),
                Some(OsStr::new("/home/u")),
            )
            .unwrap(),
            PathBuf::from("/mnt/media/Pictures/Scans")
        );
        assert_eq!(
            resolve_scans_dir_from(None, Some(OsStr::new("")), Some(OsStr::new("/home/u"))).unwrap(),
            PathBuf::from("/home/u/Pictures/Scans")
        );
        assert!(resolve_scans_dir_from(None, None, None).is_err());
    }
}
