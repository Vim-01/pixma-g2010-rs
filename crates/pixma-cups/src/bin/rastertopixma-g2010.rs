//! CUPS raster filter (`rastertopixma-g2010`) for the Canon PIXMA G2010.
//!
//! Converts CUPS/PWG raster input or standard image input into hardware-validated
//! IVEC XML + PWG Raster wire streams conforming to Canon MegaTank firmware protocol.

use pixma_cups::options::parse_cups_options;
use pixma_raster::cups_reader::{parse_image_from_memory, parse_pwg_raster, RasterPageInput};
use pixma_raster::pipeline::PrintRasterPipeline;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

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

    let mut input_data = Vec::new();
    if let Some(path) = file_arg {
        if path != "-" && !path.is_empty() {
            match File::open(path) {
                Ok(mut file) => {
                    if let Err(e) = file.read_to_end(&mut input_data) {
                        eprintln!("ERROR: Failed to read input file '{path}': {e}");
                        return ExitCode::FAILURE;
                    }
                }
                Err(e) => {
                    eprintln!("ERROR: Could not open input file '{path}': {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Err(e) = io::stdin().read_to_end(&mut input_data) {
            eprintln!("ERROR: Failed to read from stdin: {e}");
            return ExitCode::FAILURE;
        }
    } else if let Err(e) = io::stdin().read_to_end(&mut input_data) {
        eprintln!("ERROR: Failed to read from stdin: {e}");
        return ExitCode::FAILURE;
    }

    if input_data.is_empty() {
        eprintln!("ERROR: Empty print input received");
        return ExitCode::FAILURE;
    }

    let mut config = parse_cups_options(options_str, copies);
    config.job_id = job_id;

    // Parse input into raster pages
    let pages: Vec<RasterPageInput> = if input_data.starts_with(b"RaS2") {
        match parse_pwg_raster(&input_data) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ERROR: Failed to parse PWG/CUPS raster stream: {e}");
                return ExitCode::FAILURE;
            }
        }
    } else {
        match parse_image_from_memory(&input_data, Some(config.quality.base_raster_dpi())) {
            Ok(page) => vec![page],
            Err(e) => {
                eprintln!("ERROR: Input is neither valid PWG raster nor recognized image: {e}");
                return ExitCode::FAILURE;
            }
        }
    };

    if pages.is_empty() {
        eprintln!("ERROR: No printable pages extracted from input");
        return ExitCode::FAILURE;
    }

    let timestamp = format_timestamp_now();
    let compiled_stream =
        match PrintRasterPipeline::compile_ivec_bjraster3_job(&pages, &config, &timestamp) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("ERROR: Failed to compile IVEC BJRaster3 job stream: {e}");
                return ExitCode::FAILURE;
            }
        };

    let mut stdout = io::stdout();
    if let Err(e) = stdout.write_all(&compiled_stream) {
        eprintln!("ERROR: Failed to write job stream to stdout: {e}");
        return ExitCode::FAILURE;
    }
    let _ = stdout.flush();

    ExitCode::SUCCESS
}

/// Formats current UTC timestamp as 14-character string `YYYYMMDDHHMMSS`.
fn format_timestamp_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = (secs / 86400) as i64;
    let rem_secs = (secs % 86400) as u32;
    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;
    let second = rem_secs % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1024 + doe / 1460 - doe / 36524) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}")
}
