//! Integration tests for pixma-cups filter, command compiler, and backend attributes.

use pixma_core::models::{DeviceStatus, InkLevels};
use pixma_cups::backend::{format_cups_discovery_lines, format_cups_marker_attributes};
use pixma_cups::commands::{compile_cups_command_stream, parse_cups_command_file};
use pixma_cups::options::parse_cups_options;
use pixma_raster::cups_reader::RasterPageInput;
use pixma_raster::pipeline::PrintRasterPipeline;

#[test]
fn test_cups_options_and_pipeline_compilation() {
    let opts = "PageSize=A4.bl MediaType=glossygold Resolution=600dpi ColorModel=RGB Collate=True";
    let config = parse_cups_options(opts, 1);

    assert_eq!(config.copies, 1);
    assert!(config.collate);
    assert!(config.borderless.is_borderless());

    // Create a 100x100 dummy raster page
    let page = RasterPageInput::new_blank(100, 100, 600);
    let job_bytes = PrintRasterPipeline::compile_ivec_pwg_job(&[page], &config, "20261106180000")
        .expect("compile job failed");

    let text = String::from_utf8_lossy(&job_bytes);
    assert!(text.contains("<ivec:operation>StartJob</ivec:operation>"));
    assert!(text.contains("<ivec:papertype>custom-media-type-canon-6</ivec:papertype>"));
    assert!(text.contains("<ivec:borderlessprint>ON</ivec:borderlessprint>"));
    assert!(text.contains("<ivec:operation>EndJob</ivec:operation>"));
}

#[test]
fn test_all_maintenance_commands_compilation() {
    let script = r#"
#CUPS-COMMAND
Clean
CleanBlack
CleanColor
DeepClean
DeepCleanBlack
DeepCleanColor
InkFlush YES-DRAIN-INK
RollerClean
BottomPlateClean
PrintSelfTestPage
AutoAlign
HalfAutoAlign
ManualAlign
AlignmentCheck
ReportLevels
ReportStatus
"#;

    let cmds = parse_cups_command_file(script).expect("failed parsing commands");
    assert_eq!(cmds.len(), 16);

    let xml = compile_cups_command_stream(&cmds, 100);
    let s = String::from_utf8(xml).expect("valid utf8 xml");

    assert!(s.contains("<ivec:type>regular</ivec:type>"));
    assert!(s.contains("<ivec:type>deep</ivec:type>"));
    assert!(s.contains("<ivec:type>choke</ivec:type>"));
    assert!(s.contains("<ivec:target>roller</ivec:target>"));
    assert!(s.contains("<ivec:target>platen</ivec:target>"));
    assert!(s.contains("<ivec:type>nozzle_check</ivec:type>"));
    assert!(s.contains("<ivec:type>half_auto_registration</ivec:type>"));
    assert!(s.contains("<ivec:type>manual_registration</ivec:type>"));
    assert!(s.contains("<ivec:type>regi_check</ivec:type>"));
    assert!(s.contains("<ivec:operation>GetStatus</ivec:operation>"));
}

#[test]
fn test_backend_discovery_and_markers() {
    let lines = format_cups_discovery_lines();
    assert!(lines
        .iter()
        .any(|l| l.contains("pixma-usb://Canon/G2010%20series")));

    let st = DeviceStatus {
        ink_levels: InkLevels {
            pigment_black_percent: 92,
            cyan_percent: 85,
            magenta_percent: 78,
            yellow_percent: 88,
            low_ink_warning: false,
        },
        ..Default::default()
    };
    let marker_lines = format_cups_marker_attributes(&st);
    assert_eq!(marker_lines[3], "ATTR: marker-levels=92,85,78,88");
}
