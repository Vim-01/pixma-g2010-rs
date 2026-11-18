//! CUPS raster filter (`rastertopixma-g2010`) for the Canon PIXMA G2010.
//!
//! Converts CUPS/PWG raster input or standard image input into hardware-validated
//! IVEC XML + PWG Raster wire streams conforming to Canon MegaTank firmware protocol.

use pixma_cups::options::parse_cups_options;
use pixma_raster::cups_reader::DocumentPageSource;
use pixma_raster::pipeline::PrintRasterPipeline;
use std::env;
use std::fs::File;
use std::io::{self, BufReader, BufWriter};
use std::process::ExitCode;

fn run_filter(args: &[String]) -> Result<(), String> {
    // CUPS filter standard argument signature:
    // argv[0] = filter name
    // argv[1] = job-id
    // argv[2] = user
    // argv[3] = title
    // argv[4] = copies
    // argv[5] = options
    // argv[6] = filename (optional; if missing, read from stdin)
    let job_id: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let copies: u16 = args.get(4).and_then(|s| s.parse().ok()).unwrap_or(1);
    let options_str = args.get(5).map(|s| s.as_str()).unwrap_or("");
    let file_arg = args.get(6).map(|s| s.as_str());

    let mut config = parse_cups_options(options_str, copies);
    config.job_id = job_id;
    let target_dpi = Some(config.quality.base_raster_dpi());

    let stdin = io::stdin();
    let mut source = if let Some(path) = file_arg.filter(|p| !p.is_empty() && *p != "-") {
        let file = File::open(path).map_err(|e| format!("Could not open input file '{path}': {e}"))?;
        DocumentPageSource::from_reader(BufReader::with_capacity(64 * 1024, file), target_dpi)
            .map_err(|e| format!("Failed to parse document input: {e}"))?
    } else {
        DocumentPageSource::from_reader(
            BufReader::with_capacity(64 * 1024, stdin.lock()),
            target_dpi,
        )
        .map_err(|e| format!("Failed to parse document input: {e}"))?
    };

    let timestamp = pixma_core::current_utc_timestamp_14();
    let mut stdout = BufWriter::with_capacity(64 * 1024, io::stdout().lock());
    let output_pages = PrintRasterPipeline::compile_ivec_bjraster3_from_source(
        &mut source,
        &config,
        &timestamp,
        &mut stdout,
    )
    .map_err(|e| format!("Failed to compile IVEC BJRaster3 job stream: {e}"))?;

    if output_pages == 0 {
        return Err("No printable pages extracted from input".to_string());
    }

    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();
    if let Err(err) = run_filter(&args) {
        eprintln!("ERROR: {err}");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}
