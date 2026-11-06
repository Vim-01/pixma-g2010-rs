//! Comprehensive unit and integration tests for `pixma-cli`.

use clap::Parser;
use pixma_cli::args::{parse_alignment_values_csv, Cli, Commands};
use pixma_cli::commands::{align, clean, print, roller_clean, scan};
use pixma_cli::connect_client;
use pixma_cli::status::{format_cartridge_state, render_ascii_status, render_progress_bar};
use pixma_core::maintenance::{
    MaintenanceRecord, MaintenanceSupervisor, INK_FLUSH_CONFIRMATION_TOKEN,
};
use pixma_core::models::{CartridgeState, DeviceStatus, InkLevels, OperationalState, PrinterModel};
use pixma_daemon::DaemonClient;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use std::path::PathBuf;

#[test]
fn test_cli_parse_status() {
    let args = vec!["pixma-cli", "--mock", "--json", "status"];
    let cli = Cli::try_parse_from(args).expect("Failed to parse status");
    assert!(cli.mock);
    assert!(cli.json);
    assert_eq!(cli.command, Commands::Status);
}

#[test]
fn test_cli_parse_clean_regular_and_flush() {
    let args = vec![
        "pixma-cli",
        "clean",
        "--kind",
        "deep",
        "--target",
        "color",
        "--override-cooldown-dangerously",
    ];
    let cli = Cli::try_parse_from(args).expect("Failed to parse clean deep");
    match cli.command {
        Commands::Clean {
            kind,
            target,
            confirm,
            override_cooldown_dangerously,
        } => {
            assert_eq!(kind, pixma_cli::args::CleanKindArg::Deep);
            assert_eq!(target, pixma_cli::args::CleanTargetArg::Color);
            assert_eq!(confirm, None);
            assert!(override_cooldown_dangerously);
        }
        _ => panic!("Expected Clean command"),
    }

    let args_flush = vec![
        "pixma-cli",
        "clean",
        "--kind",
        "flush",
        "--confirm",
        "YES-DRAIN-INK",
    ];
    let cli_flush = Cli::try_parse_from(args_flush).expect("Failed to parse clean flush");
    match cli_flush.command {
        Commands::Clean { kind, confirm, .. } => {
            assert_eq!(kind, pixma_cli::args::CleanKindArg::Flush);
            assert_eq!(confirm.as_deref(), Some("YES-DRAIN-INK"));
        }
        _ => panic!("Expected Clean command"),
    }
}

#[test]
fn test_cli_parse_roller_clean() {
    let args = vec!["pixma-cli", "roller-clean", "--target", "platen"];
    let cli = Cli::try_parse_from(args).expect("Failed to parse roller-clean");
    match cli.command {
        Commands::RollerClean { target } => {
            assert_eq!(target, pixma_cli::args::RollerTargetArg::Platen);
        }
        _ => panic!("Expected RollerClean"),
    }
}

#[test]
fn test_cli_parse_nozzle_check() {
    let args = vec!["pixma-cli", "nozzle-check"];
    let cli = Cli::try_parse_from(args).expect("Failed to parse nozzle-check");
    assert_eq!(cli.command, Commands::NozzleCheck);
}

#[test]
fn test_cli_parse_align_and_csv_validation() {
    let args = vec![
        "pixma-cli",
        "align",
        "--action",
        "apply",
        "--values",
        "0,1,-1,2,-2,3,-3,4,-4,5,-5,0,0,1",
    ];
    let cli = Cli::try_parse_from(args).expect("Failed to parse align");
    match cli.command {
        Commands::Align { action, values } => {
            assert_eq!(action, pixma_cli::args::AlignActionArg::Apply);
            assert_eq!(values.as_deref(), Some("0,1,-1,2,-2,3,-3,4,-4,5,-5,0,0,1"));
            let parsed = parse_alignment_values_csv(values.as_deref().unwrap()).unwrap();
            assert_eq!(parsed.len(), 14);
            assert_eq!(parsed[0], 0);
            assert_eq!(parsed[1], 1);
            assert_eq!(parsed[2], -1);
        }
        _ => panic!("Expected Align"),
    }

    // Test rejection of invalid counts or values
    assert!(parse_alignment_values_csv("0,1,2").is_err());
    assert!(parse_alignment_values_csv("0,1,2,3,4,5,6,7,8,9,10,11,12,13").is_err()); // 6 > 5
    assert!(parse_alignment_values_csv("0,1,2,3,4,-6,0,0,0,0,0,0,0,0").is_err()); // -6 < -5
    assert!(parse_alignment_values_csv("a,b,c,d,e,f,g,h,i,j,k,l,m,n").is_err());
}

#[test]
fn test_cli_parse_scan_flags() {
    let args = vec![
        "pixma-cli",
        "scan",
        "--dpi",
        "600",
        "--mode",
        "gray",
        "--output",
        "/tmp/document.pdf",
        "--auto-crop",
        "--deskew",
        "--descreen",
        "--unsharp",
        "--remove-dust",
        "--gutter-shadow",
        "--gamma",
        "1.8",
    ];
    let cli = Cli::try_parse_from(args).expect("Failed to parse scan");
    match cli.command {
        Commands::Scan {
            dpi,
            mode,
            output,
            auto_crop,
            deskew,
            descreen,
            unsharp,
            remove_dust,
            gutter_shadow,
            gamma,
            ..
        } => {
            assert_eq!(dpi, 600);
            assert_eq!(mode, pixma_cli::args::ScanModeArg::Gray);
            assert_eq!(output, PathBuf::from("/tmp/document.pdf"));
            assert!(auto_crop);
            assert!(deskew);
            assert!(descreen);
            assert!(unsharp);
            assert!(remove_dust);
            assert!(gutter_shadow);
            assert!((gamma - 1.8).abs() < 1e-4);
        }
        _ => panic!("Expected Scan"),
    }
}

#[test]
fn test_cli_parse_print_flags() {
    let args = vec![
        "pixma-cli",
        "print",
        "--test-page",
        "--media",
        "glossygold",
        "--paper",
        "4x6",
        "--quality",
        "2",
        "--borderless",
        "2",
        "--layout",
        "fit",
        "--grayscale",
        "--cyan",
        "15",
        "--magenta",
        "-10",
        "--yellow",
        "5",
        "--brightness",
        "-12",
        "--contrast",
        "8",
        "--wide-gap",
        "--drying",
        "long",
        "--quiet",
    ];
    let cli = Cli::try_parse_from(args).expect("Failed to parse print");
    match cli.command {
        Commands::Print {
            test_page,
            media,
            paper,
            quality,
            borderless,
            layout,
            grayscale,
            cyan,
            magenta,
            yellow,
            brightness,
            contrast,
            wide_gap,
            drying,
            quiet,
            ..
        } => {
            assert!(test_page);
            assert_eq!(media, pixma_cli::args::MediaTypeArg::Glossygold);
            assert_eq!(paper, pixma_cli::args::PaperSizeArg::Photo4x6);
            assert_eq!(quality, 2);
            assert_eq!(borderless, pixma_cli::args::BorderlessArg::Level2);
            assert_eq!(layout, pixma_cli::args::LayoutArg::Fit);
            assert!(grayscale);
            assert_eq!(cyan, 15);
            assert_eq!(magenta, -10);
            assert_eq!(yellow, 5);
            assert_eq!(brightness, -12);
            assert_eq!(contrast, 8);
            assert!(wide_gap);
            assert_eq!(drying, pixma_cli::args::DryingArg::Long);
            assert!(quiet);
        }
        _ => panic!("Expected Print"),
    }
}

#[test]
fn test_ascii_status_rendering() {
    let status = DeviceStatus {
        model: PrinterModel::G2010,
        serial_number: "KMNT12345".to_string(),
        firmware_version: "1.020".to_string(),
        state: OperationalState::Idle,
        status_detail: "Ready".to_string(),
        support_code: Some("5200".to_string()),
        lcd_code: Some("P08".to_string()),
        ink_levels: InkLevels {
            pigment_black_percent: 85,
            cyan_percent: 70,
            magenta_percent: 50,
            yellow_percent: 20,
            low_ink_warning: false,
        },
        black_head_state: CartridgeState::InstalledOk,
        color_head_state: CartridgeState::InstalledOk,
        waste_absorber_percent: 18.5,
    };

    let rendered = render_ascii_status(&status);
    assert!(rendered.contains("Canon PIXMA G2010 series Telemetry Report"));
    assert!(rendered.contains("KMNT12345"));
    assert!(rendered.contains("Pigment Black (PGBK)"));
    assert!(rendered.contains("85%"));
    assert!(rendered.contains("18.5%"));
    assert!(rendered.contains("OK (Installed)"));
    assert!(rendered.contains("5200"));
    assert!(rendered.contains("P08"));
    assert!(rendered.contains("Print Head Overheating Protection Triggered"));
    assert!(rendered.contains("Thermal / Printhead Hazard"));
}

#[test]
fn test_ascii_progress_bar() {
    let empty = render_progress_bar(0.0, 10);
    assert_eq!(empty, "[░░░░░░░░░░]");
    let full = render_progress_bar(100.0, 10);
    assert_eq!(full, "[██████████]");
    let half = render_progress_bar(50.0, 10);
    assert_eq!(half, "[█████░░░░░]");
}

#[test]
fn test_cartridge_formatting() {
    assert_eq!(
        format_cartridge_state(CartridgeState::InstalledOk),
        "OK (Installed)"
    );
    assert_eq!(
        format_cartridge_state(CartridgeState::Overheated),
        "CRITICAL: Overheated"
    );
}

#[tokio::test]
async fn test_maintenance_safety_interlock_ink_flush_requires_token() {
    let mock = MockG2010Transport::new();
    let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
    let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let mut client = DaemonClient::direct(ctrl);

    // Call clean flush without token
    let result = clean::execute_clean(
        &mut client,
        pixma_cli::args::CleanKindArg::Flush,
        pixma_cli::args::CleanTargetArg::All,
        None,
        false,
        false,
    )
    .await;

    assert!(result.is_err());
    let err_msg = format!("{:#}", result.err().unwrap());
    assert!(err_msg.contains("Confirmation required for Ink Flush"));
}

#[tokio::test]
async fn test_maintenance_safety_interlock_ink_flush_absorber_capacity() {
    let mock = MockG2010Transport::new();
    // Absorber already at 82%
    let record = MaintenanceRecord {
        estimated_d_counter_percent: 82.0,
        ..Default::default()
    };
    let supervisor = MaintenanceSupervisor::in_memory(record);
    let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let mut client = DaemonClient::direct(ctrl);

    // Call clean flush with valid token, but D-Counter is >= 80%
    let result = clean::execute_clean(
        &mut client,
        pixma_cli::args::CleanKindArg::Flush,
        pixma_cli::args::CleanTargetArg::All,
        Some(INK_FLUSH_CONFIRMATION_TOKEN),
        false,
        false,
    )
    .await;

    assert!(result.is_err());
    let err_msg = format!("{:#}", result.err().unwrap());
    assert!(err_msg.contains("Waste ink absorber hazard") || err_msg.contains("82.0%"));
}

#[tokio::test]
async fn test_maintenance_cooldown_interlock_regular_clean() {
    let mock = MockG2010Transport::new();
    let now = MaintenanceSupervisor::current_unix_epoch();
    // Last clean executed 10 seconds ago
    let record = MaintenanceRecord {
        last_standard_clean_epoch: now - 10,
        ..Default::default()
    };
    let supervisor = MaintenanceSupervisor::in_memory(record);
    let ctrl = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let mut client = DaemonClient::direct(ctrl);

    // Should fail because 60-second cooldown is active
    let result = clean::execute_clean(
        &mut client,
        pixma_cli::args::CleanKindArg::Regular,
        pixma_cli::args::CleanTargetArg::All,
        None,
        false,
        false,
    )
    .await;

    assert!(result.is_err());
    let err_msg = format!("{:#}", result.err().unwrap());
    assert!(err_msg.contains("Safety cooldown active") || err_msg.contains("Standard Cleaning"));

    // Now with override_cooldown = true, it must succeed!
    let override_res = clean::execute_clean(
        &mut client,
        pixma_cli::args::CleanKindArg::Regular,
        pixma_cli::args::CleanTargetArg::All,
        None,
        true,
        false,
    )
    .await;

    assert!(override_res.is_ok());
}

#[tokio::test]
async fn test_headless_scan_execution_on_mock() {
    let mut client = connect_client(None, true).await.unwrap();

    let tmp_dir = std::env::temp_dir();
    let output_file = tmp_dir.join(format!("test_scan_{}.png", std::process::id()));

    let res = scan::execute_scan(
        &mut client,
        300,
        pixma_cli::args::ScanModeArg::Color,
        &output_file,
        Some(300),
        Some(32),
        false,
        false,
        false,
        false,
        false,
        false,
        2.2,
        false,
    )
    .await;

    assert!(res.is_ok(), "Scan execution failed: {:?}", res.err());
    assert!(output_file.exists(), "Output scan file was not created");

    // Clean up
    let _ = std::fs::remove_file(&output_file);
}

#[tokio::test]
async fn test_headless_print_test_page_execution_on_mock() {
    let mut client = connect_client(None, true).await.unwrap();

    let res = print::execute_print(
        &mut client,
        None,
        true, // test-page
        pixma_cli::args::MediaTypeArg::Plain,
        pixma_cli::args::PaperSizeArg::A4,
        3, // Quality 3
        pixma_cli::args::BorderlessArg::Off,
        pixma_cli::args::LayoutArg::Normal,
        false,
        0,
        0,
        0,
        0,
        0,
        false,
        pixma_cli::args::DryingArg::Normal,
        false,
        false,
    )
    .await;

    assert!(res.is_ok(), "Print execution failed: {:?}", res.err());
}

#[tokio::test]
async fn test_headless_roller_clean_and_align_on_mock() {
    let mut client = connect_client(None, true).await.unwrap();

    let roller_res = roller_clean::execute_roller_clean(
        &mut client,
        pixma_cli::args::RollerTargetArg::Roller,
        false,
    )
    .await;
    assert!(roller_res.is_ok());

    let align_res = align::execute_align(
        &mut client,
        pixma_cli::args::AlignActionArg::Check,
        None,
        false,
    )
    .await;
    assert!(align_res.is_ok());
}
