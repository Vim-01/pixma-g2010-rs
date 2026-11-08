//! Direct raster printing execution and document ingestion.

use pixma_core::print_params::{
    BorderlessExtension, ColorAdjustment, DuplexMode, InkDryingTime, MediaType, PageLayout,
    PaperGapMode, PaperSize, PrintJobConfig, PrintQuality, QuietMode,
};
use pixma_daemon::DaemonClient;
use pixma_protocol::pwg_raster::PWG_RASTER_MAGIC;
use pixma_raster::{
    generate_test_print_page, parse_image_from_memory, parse_pwg_raster, PrintRasterPipeline,
    RasterPageInput,
};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::args::{
    parse_paper_gap_mode, parse_quality_level, parse_quiet_mode, BorderlessArg, DryingArg,
    LayoutArg, MediaTypeArg, PaperSizeArg,
};

/// Computes a standard 14-digit UTC timestamp `YYYYMMDDHHMMSS` from system clock.
pub fn current_timestamp_14() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let days = (now / 86400) as i64;
    let rem_secs = (now % 86400) as u32;
    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;
    let second = rem_secs % 60;

    // Gregorian civil date calculation from Euclidean Affine
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
    let pages: Vec<RasterPageInput> = if test_page {
        if !json {
            println!(
                "Generating internal diagnostic test page at {} DPI...",
                quality.base_raster_dpi()
            );
        }
        vec![generate_test_print_page(quality.base_raster_dpi())]
    } else if let Some(path) = input_path {
        let file_bytes = std::fs::read(path)
            .map_err(|e| anyhow::anyhow!("Failed to read input file {}: {e}", path.display()))?;

        if file_bytes.starts_with(&PWG_RASTER_MAGIC) {
            if !json {
                println!("Parsing input PWG Raster stream (RaS2)...");
            }
            parse_pwg_raster(&file_bytes)
                .map_err(|e| anyhow::anyhow!("Failed to parse PWG Raster: {e}"))?
        } else {
            if !json {
                println!("Decoding input image format...");
            }
            let page = parse_image_from_memory(&file_bytes, Some(quality.base_raster_dpi()))
                .map_err(|e| anyhow::anyhow!("Failed to decode image file: {e}"))?;
            vec![page]
        }
    } else {
        return Err(anyhow::anyhow!(
            "Either `--input <PATH>` or `--test-page` must be specified for print."
        ));
    };

    if !json {
        println!(
            "Rendering and compiling print job: {} page(s), {:?}, {:?}, quality {:?}...",
            pages.len(),
            media_type,
            paper_size,
            quality
        );
    }

    let timestamp = current_timestamp_14();
    let job_stream = PrintRasterPipeline::compile_ivec_bjraster3_job(&pages, &config, &timestamp)
        .map_err(|e| anyhow::anyhow!("Raster pipeline compilation failed: {e}"))?;

    if !json {
        println!(
            "Compiled {} bytes of wire data. Streaming to printer Bulk OUT pipe...",
            job_stream.len()
        );
    }

    client.print_raw_stream(&job_stream).await?;

    if json {
        let result = serde_json::json!({
            "status": "success",
            "pages_printed": pages.len(),
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
