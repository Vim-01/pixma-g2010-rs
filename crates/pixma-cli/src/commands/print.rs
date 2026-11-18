//! Direct raster printing execution and document ingestion.

use pixma_core::print_params::{
    BorderlessExtension, ColorAdjustment, DuplexMode, InkDryingTime, MediaType, PageLayout,
    PaperGapMode, PaperSize, PrintJobConfig, PrintQuality, QuietMode,
};
use pixma_daemon::DaemonClient;
use pixma_raster::{generate_test_print_page, DocumentPageSource, PrintRasterPipeline};
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::args::{
    parse_paper_gap_mode, parse_quality_level, parse_quiet_mode, BorderlessArg, DryingArg,
    LayoutArg, MediaTypeArg, PaperSizeArg,
};

/// Computes a standard 14-digit UTC timestamp `YYYYMMDDHHMMSS` from system clock.
pub fn current_timestamp_14() -> String {
    pixma_core::current_utc_timestamp_14()
}

/// Executes print job compilation and streaming to the printer endpoint.
#[allow(clippy::too_many_arguments)]
pub async fn execute_print(
    client: &mut DaemonClient,
    input_path: Option<&Path>,
    test_page: bool,
    media_arg: MediaTypeArg,
    paper_arg: PaperSizeArg,
    quality_int: u8,
    borderless_arg: BorderlessArg,
    layout_arg: LayoutArg,
    grayscale: bool,
    cyan: i8,
    magenta: i8,
    yellow: i8,
    brightness: i8,
    contrast: i8,
    wide_gap: bool,
    drying_arg: DryingArg,
    quiet: bool,
    json: bool,
) -> anyhow::Result<()> {
    let media_type: MediaType = media_arg.into();
    let paper_size: PaperSize = paper_arg.into();
    let quality: PrintQuality = parse_quality_level(quality_int);
    let borderless: BorderlessExtension = borderless_arg.into();
    let layout: PageLayout = layout_arg.into();
    let paper_gap: PaperGapMode = parse_paper_gap_mode(wide_gap);
    let drying_time: InkDryingTime = drying_arg.into();
    let quiet_mode: QuietMode = parse_quiet_mode(quiet);

    let color = ColorAdjustment {
        cyan_balance: cyan,
        magenta_balance: magenta,
        yellow_balance: yellow,
        brightness,
        contrast,
        intensity: 0,
        gamma: 2.2,
        grayscale,
    };

    let job_id = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        % 1_000_000
        + 1) as u32;

    let config = PrintJobConfig {
        job_id,
        media_type,
        paper_size,
        quality,
        borderless,
        color,
        layout,
        duplex: DuplexMode::Off,
        drying_time,
        paper_gap,
        quiet_mode,
        copies: 1,
        collate: false,
        reverse_order: false,
    };

    // Notice glossy composite CMY black restriction
    if media_type.requires_composite_cmy_black() && !json {
        println!(
            "Notice: Composite CMY Mode Active (Pigment Black PGBK disabled to protect glossy coating)"
        );
    }

    // Ingest pages
    let timestamp = current_timestamp_14();
    let (pages_printed, job_stream) = if test_page {
        if !json {
            println!(
                "Generating internal diagnostic test page at {} DPI...",
                quality.base_raster_dpi()
            );
            println!(
                "Streaming and compiling diagnostic test page, {:?}, {:?}, quality {:?}...",
                media_type, paper_size, quality
            );
        }
        let page = generate_test_print_page(quality.base_raster_dpi());
        let mut source = DocumentPageSource::from_page(page);
        let mut stream = Vec::new();
        let count = PrintRasterPipeline::compile_ivec_bjraster3_from_source(
            &mut source,
            &config,
            &timestamp,
            &mut stream,
        )
        .map_err(|e| anyhow::anyhow!("Raster pipeline compilation failed: {e}"))?;
        (count, stream)
    } else if let Some(path) = input_path {
        if !json {
            println!(
                "Streaming and compiling print job from {}, {:?}, {:?}, quality {:?}...",
                path.display(),
                media_type,
                paper_size,
                quality
            );
        }
        let file = File::open(path)
            .map_err(|e| anyhow::anyhow!("Failed to open input file {}: {e}", path.display()))?;
        let mut source = DocumentPageSource::from_reader(
            BufReader::with_capacity(64 * 1024, file),
            Some(quality.base_raster_dpi()),
        )
        .map_err(|e| anyhow::anyhow!("Failed to decode document file: {e}"))?;
        let mut stream = Vec::new();
        let count = PrintRasterPipeline::compile_ivec_bjraster3_from_source(
            &mut source,
            &config,
            &timestamp,
            &mut stream,
        )
        .map_err(|e| anyhow::anyhow!("Raster pipeline compilation failed: {e}"))?;
        (count, stream)
    } else {
        return Err(anyhow::anyhow!(
            "Either `--input <PATH>` or `--test-page` must be specified for print."
        ));
    };

    if !json {
        println!(
            "Compiled {} bytes ({} output page(s)) of wire data. Streaming to printer Bulk OUT pipe...",
            job_stream.len(),
            pages_printed
        );
    }

    client.print_raw_stream(&job_stream).await?;

    if json {
        let result = serde_json::json!({
            "status": "success",
            "pages_printed": pages_printed,
            "media_type": format!("{:?}", media_type),
            "paper_size": format!("{:?}", paper_size),
            "quality": quality_int,
            "bytes_streamed": job_stream.len(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Print job successfully completed! Page ejected.");
    }

    Ok(())
}
