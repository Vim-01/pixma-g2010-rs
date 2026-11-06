//! Pure-Rust, headlessly testable state and view-model for PIXMA G2010 Studio.

use image::RgbImage;
use pixma_core::errors::{SafetyInterlockError, SupportCodeInfo};
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor,
    RollerCleanTarget, TestPrintType, INK_FLUSH_CONFIRMATION_TOKEN, MAX_SAFE_D_COUNTER_PERCENT,
};
use pixma_core::models::DeviceStatus;
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintJobConfig, PrintQuality, QuietMode,
};
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_daemon::DaemonClient;
use pixma_imageproc::execute_scan_pipeline;
use pixma_raster::{generate_test_print_page, PrintRasterPipeline};
use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

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
            maintenance_record,
            alignment_values: [0; 14],
            ink_flush_confirmed: false,
            glossy_notification: None,
            status_banner: String::from("Device Ready"),
            last_error: None,
            is_busy: false,
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

    // Status & telemetry
    UpdateDeviceStatus(DeviceStatus),
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
                        self.status_banner = String::from("Device Ready");
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

            // Status updates
            StudioEvent::UpdateDeviceStatus(status) => {
                self.device_status = status;
                self.update_status_banner_from_device();
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

    /// Updates status banner based on active support codes or device operational state.
    fn update_status_banner_from_device(&mut self) {
        if let Some(ref code) = self.device_status.support_code {
            if let Some(info) = SupportCodeInfo::lookup(code) {
                self.status_banner = format!(
                    "[ALERT] Support Code {} (LCD: {}): {}",
                    info.support_code, info.lcd_code, info.title
                );
            } else {
                self.status_banner = format!("[ALERT] Active Support Code: {code}");
            }
        } else if let Some(ref notice) = self.glossy_notification {
            self.status_banner = notice.clone();
        } else {
            self.status_banner = format!("Device Ready ({:?})", self.device_status.state);
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

        let raw_img = match scan_params.color_mode {
            ScanColorMode::Color24Bit => {
                let expected = (width as usize) * (height as usize) * 3;
                let mut data = raw_bytes;
                if data.len() < expected {
                    data.resize(expected, 255);
                }
                data.truncate(expected);
                RgbImage::from_raw(width, height, data)
                    .ok_or_else(|| "Failed to decode RGB scan buffer".to_string())?
            }
            ScanColorMode::Grayscale8Bit | ScanColorMode::Lineart1Bit => {
                let expected = (width as usize) * (height as usize);
                let mut data = raw_bytes;
                if data.len() < expected {
                    data.resize(expected, 255);
                }
                data.truncate(expected);
                let mut rgb = Vec::with_capacity(expected * 3);
                for b in data {
                    rgb.push(b);
                    rgb.push(b);
                    rgb.push(b);
                }
                RgbImage::from_raw(width, height, rgb)
                    .ok_or_else(|| "Failed to decode Gray scan buffer".to_string())?
            }
        };

        let processed = execute_scan_pipeline(raw_img, &scan_params).map_err(|e| e.to_string())?;

        self.is_busy = false;
        self.last_error = None;
        Ok(processed)
    }

    /// Renders and prints an internal diagnostic test page.
    pub async fn dispatch_print_test_page(
        &mut self,
        client: &mut DaemonClient,
    ) -> Result<(), String> {
        self.is_busy = true;

        let dpi = self.print_config.quality.base_raster_dpi();
        let page = generate_test_print_page(dpi);
        let timestamp = Self::current_timestamp_14();

        let wire_data =
            PrintRasterPipeline::compile_ivec_pwg_job(&[page], &self.print_config, &timestamp)
                .map_err(|e| e.to_string())?;

        client
            .print_raw_stream(&wire_data)
            .await
            .map_err(|e| e.to_string())?;

        self.is_busy = false;
        self.last_error = None;
        Ok(())
    }

    /// 14-digit UTC timestamp helper.
    fn current_timestamp_14() -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let days = (now / 86400) as i64;
        let rem_secs = (now % 86400) as u32;
        let hour = rem_secs / 3600;
        let minute = (rem_secs % 3600) / 60;
        let second = rem_secs % 60;

        let z = days + 719468;
        let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
        let doe = (z - era * 146097) as u32;
        let yoe = (doe - doe / 1024 + doe / 1461 - doe / 146096) / 365;
        let y = (yoe as i64) + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        let y = if m <= 2 { y + 1 } else { y };

        format!("{y:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}")
    }
}

/// Headless self-test runner for CI and containerized environments.
pub async fn run_self_test() -> Result<(), String> {
    use pixma_core::models::PrinterModel;
    use pixma_transport::controller::{AnyTransport, G2010Controller};
    use pixma_transport::mock_device::MockG2010Transport;

    println!("[Self-Test] Initializing StudioViewModel...");
    let mut model = StudioViewModel::default();

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
