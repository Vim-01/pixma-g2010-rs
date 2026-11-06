//! Comprehensive tests for `pixma-gui` view-model, state transitions,
//! safety interlocks, and mock execution.

use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceRecord, MaintenanceSupervisor,
};
use pixma_core::models::{DeviceStatus, OperationalState, PrinterModel};
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintQuality, QuietMode,
};
use pixma_core::scan_params::{ScanColorMode, ScanResolution};
use pixma_daemon::DaemonClient;
use pixma_gui::state::{run_self_test, StudioEvent, StudioViewModel, GLOSSY_COMPOSITE_CMY_NOTICE};
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;

#[test]
fn test_view_model_initial_state() {
    let vm = StudioViewModel::default();
    assert_eq!(vm.print_config.media_type, MediaType::Plain);
    assert_eq!(vm.print_config.paper_size, PaperSize::A4);
    assert_eq!(vm.print_config.quality, PrintQuality::Level3Standard);
    assert_eq!(vm.print_config.borderless, BorderlessExtension::Off);
    assert_eq!(vm.print_config.layout, PageLayout::Normal);
    assert!(!vm.print_config.color.grayscale);
    assert_eq!(vm.scan_params.resolution, ScanResolution::Dpi300);
    assert_eq!(vm.scan_params.color_mode, ScanColorMode::Color24Bit);
    assert_eq!(vm.alignment_values, [0; 14]);
    assert!(!vm.ink_flush_confirmed);
    assert_eq!(vm.glossy_notification, None);
    assert!(!vm.is_busy);
}

#[test]
fn test_glossy_media_notification_lifecycle() {
    let mut vm = StudioViewModel::default();

    // Selecting Glossy photo paper must trigger composite CMY notice
    vm.handle_event(StudioEvent::SetMediaType(MediaType::GlossyPhoto));
    assert!(vm.print_config.media_type.requires_composite_cmy_black());
    assert_eq!(
        vm.glossy_notification.as_deref(),
        Some(GLOSSY_COMPOSITE_CMY_NOTICE)
    );
    assert!(vm.ui_print_summary().contains(GLOSSY_COMPOSITE_CMY_NOTICE));
    assert_eq!(vm.status_banner, GLOSSY_COMPOSITE_CMY_NOTICE);

    // Test other glossy papers requiring composite CMY
    let glossy_variants = [
        MediaType::PhotoPlusGlossyII,
        MediaType::PhotoProPlatinum,
        MediaType::PhotoProLuster,
        MediaType::PhotoPlusSemiGloss,
    ];
    for g in glossy_variants {
        vm.handle_event(StudioEvent::SetMediaType(g));
        assert!(vm.print_config.media_type.requires_composite_cmy_black());
        assert_eq!(
            vm.glossy_notification.as_deref(),
            Some(GLOSSY_COMPOSITE_CMY_NOTICE)
        );
    }

    // Switching back to Plain paper must clear notification
    vm.handle_event(StudioEvent::SetMediaType(MediaType::Plain));
    assert!(!vm.print_config.media_type.requires_composite_cmy_black());
    assert_eq!(vm.glossy_notification, None);
    assert!(!vm.ui_print_summary().contains(GLOSSY_COMPOSITE_CMY_NOTICE));

    // Matte photo does not disable PGBK
    vm.handle_event(StudioEvent::SetMediaType(MediaType::MattePhoto));
    assert!(!vm.print_config.media_type.requires_composite_cmy_black());
    assert_eq!(vm.glossy_notification, None);
}

#[test]
fn test_print_settings_state_transitions() {
    let mut vm = StudioViewModel::default();

    vm.handle_event(StudioEvent::SetPaperSize(PaperSize::Photo4x6));
    assert_eq!(vm.print_config.paper_size, PaperSize::Photo4x6);

    vm.handle_event(StudioEvent::SetPrintQuality(PrintQuality::Level1Fine));
    assert_eq!(vm.print_config.quality, PrintQuality::Level1Fine);

    vm.handle_event(StudioEvent::SetBorderless(
        BorderlessExtension::Extension2Med,
    ));
    assert_eq!(
        vm.print_config.borderless,
        BorderlessExtension::Extension2Med
    );

    vm.handle_event(StudioEvent::SetPageLayout(PageLayout::NUp {
        pages_per_sheet: 4,
    }));
    assert_eq!(
        vm.print_config.layout,
        PageLayout::NUp { pages_per_sheet: 4 }
    );

    vm.handle_event(StudioEvent::SetGrayscale(true));
    assert!(vm.print_config.color.grayscale);

    vm.handle_event(StudioEvent::SetCyanBalance(25));
    assert_eq!(vm.print_config.color.cyan_balance, 25);
    vm.handle_event(StudioEvent::SetCyanBalance(60)); // Clamped to 50
    assert_eq!(vm.print_config.color.cyan_balance, 50);

    vm.handle_event(StudioEvent::SetMagentaBalance(-30));
    assert_eq!(vm.print_config.color.magenta_balance, -30);
    vm.handle_event(StudioEvent::SetMagentaBalance(-60)); // Clamped to -50
    assert_eq!(vm.print_config.color.magenta_balance, -50);

    vm.handle_event(StudioEvent::SetBrightness(10));
    assert_eq!(vm.print_config.color.brightness, 10);

    vm.handle_event(StudioEvent::SetContrast(-15));
    assert_eq!(vm.print_config.color.contrast, -15);

    vm.handle_event(StudioEvent::SetPaperGap(PaperGapMode::WidePreventAbrasion));
    assert_eq!(vm.print_config.paper_gap, PaperGapMode::WidePreventAbrasion);

    vm.handle_event(StudioEvent::SetDryingTime(InkDryingTime::Long));
    assert_eq!(vm.print_config.drying_time, InkDryingTime::Long);

    vm.handle_event(StudioEvent::SetQuietMode(QuietMode::AlwaysOn));
    assert_eq!(vm.print_config.quiet_mode, QuietMode::AlwaysOn);
}

#[test]
fn test_scanner_settings_state_transitions() {
    let mut vm = StudioViewModel::default();

    vm.handle_event(StudioEvent::SetScanResolution(ScanResolution::Dpi600));
    assert_eq!(vm.scan_params.resolution, ScanResolution::Dpi600);

    vm.handle_event(StudioEvent::SetScanColorMode(ScanColorMode::Grayscale8Bit));
    assert_eq!(vm.scan_params.color_mode, ScanColorMode::Grayscale8Bit);

    vm.handle_event(StudioEvent::SetScanAutoCrop(true));
    assert!(vm.scan_params.auto_multi_crop);

    vm.handle_event(StudioEvent::SetScanDeskew(true));
    assert!(vm.scan_params.deskew);

    vm.handle_event(StudioEvent::SetScanDescreen(true));
    assert!(vm.scan_params.descreen_moire);

    vm.handle_event(StudioEvent::SetScanUnsharp(false));
    assert!(!vm.scan_params.unsharp_mask);

    vm.handle_event(StudioEvent::SetScanRemoveDust(true));
    assert!(vm.scan_params.remove_dust_scratches);

    vm.handle_event(StudioEvent::SetScanGutterShadow(true));
    assert!(vm.scan_params.correct_gutter_shadow);

    vm.handle_event(StudioEvent::SetScanGamma(1.8));
    assert!((vm.scan_params.gamma - 1.8).abs() < 1e-4);
}

#[test]
fn test_alignment_column_values_clamping() {
    let mut vm = StudioViewModel::default();

    vm.handle_event(StudioEvent::SetAlignmentColumn(0, 4));
    assert_eq!(vm.alignment_values[0], 4);

    vm.handle_event(StudioEvent::SetAlignmentColumn(13, -4));
    assert_eq!(vm.alignment_values[13], -4);

    // Clamping to -5..=5
    vm.handle_event(StudioEvent::SetAlignmentColumn(1, 10));
    assert_eq!(vm.alignment_values[1], 5);
    vm.handle_event(StudioEvent::SetAlignmentColumn(2, -10));
    assert_eq!(vm.alignment_values[2], -5);

    // Out of bounds column index ignored safely
    vm.handle_event(StudioEvent::SetAlignmentColumn(20, 3));

    let table = [1, -1, 2, -2, 3, -3, 4, -4, 5, -5, 0, 0, 1, -1];
    vm.handle_event(StudioEvent::SetAllAlignmentValues(table));
    assert_eq!(vm.alignment_values, table);
}

#[tokio::test]
async fn test_ink_flush_safety_interlocks_on_view_model() {
    let mut vm = StudioViewModel::default();
    let mock = MockG2010Transport::new();
    let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
    let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let mut client = DaemonClient::direct(ctrl);

    // 1. Flush blocked when unconfirmed
    assert!(!vm.ink_flush_confirmed);
    let res = vm
        .dispatch_cleaning(
            &mut client,
            CleaningType::InkFlush,
            CleaningTarget::All,
            false,
        )
        .await;
    assert!(res.is_err());
    assert!(vm
        .last_error
        .as_deref()
        .unwrap()
        .contains("Confirmation required for Ink Flush"));

    // 2. Flush blocked when D-Counter >= 80% even if confirmed
    vm.handle_event(StudioEvent::SetInkFlushConfirmation(true));
    vm.device_status.waste_absorber_percent = 81.5;
    let res_absorber = vm
        .dispatch_cleaning(
            &mut client,
            CleaningType::InkFlush,
            CleaningTarget::All,
            false,
        )
        .await;
    assert!(res_absorber.is_err());
    assert!(vm
        .last_error
        .as_deref()
        .unwrap()
        .contains("Waste ink absorber hazard"));

    // 3. Reset absorber and test successful execution
    vm.device_status.waste_absorber_percent = 10.0;
    vm.handle_event(StudioEvent::SetInkFlushConfirmation(true));
    let res_ok = vm
        .dispatch_cleaning(
            &mut client,
            CleaningType::InkFlush,
            CleaningTarget::All,
            false,
        )
        .await;
    assert!(res_ok.is_ok());
    // Confirmation toggle must automatically reset to false after execution
    assert!(!vm.ink_flush_confirmed);
}

#[tokio::test]
async fn test_telemetry_banner_support_code_display() {
    let mut vm = StudioViewModel::default();
    let mut status = DeviceStatus {
        model: PrinterModel::G2010,
        state: OperationalState::OperatorCall,
        support_code: Some("1401".to_string()),
        lcd_code: Some("E05".to_string()),
        ..Default::default()
    };

    vm.handle_event(StudioEvent::UpdateDeviceStatus(status.clone()));
    assert!(vm.status_banner.contains("1401"));
    assert!(vm.status_banner.contains("E05"));
    assert!(vm
        .status_banner
        .contains("Print Head Not Recognized / Contact Fault"));

    // Clearing support code restores ready banner
    status.support_code = None;
    status.lcd_code = None;
    status.state = OperationalState::Idle;
    vm.handle_event(StudioEvent::UpdateDeviceStatus(status));
    assert!(vm.status_banner.contains("Device Ready"));
}

#[tokio::test]
async fn test_gui_self_test_runner() {
    let res = run_self_test().await;
    assert!(res.is_ok(), "run_self_test failed: {:?}", res.err());
}
