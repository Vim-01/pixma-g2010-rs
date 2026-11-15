//! CUPS raster filter (`rastertopixma-g2010`) for the Canon PIXMA G2010.
//!
//! Converts CUPS/PWG raster input or standard image input into hardware-validated
//! IVEC XML + PWG Raster wire streams conforming to Canon MegaTank firmware protocol.

use pixma_cups::options::parse_cups_options;
use pixma_raster::cups_reader::{parse_document_from_memory, RasterPageInput};
use pixma_raster::pipeline::PrintRasterPipeline;
use std::env;
use std::fs::File;
use std::io::{self, Read, Write};
use std::process::ExitCode;

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

    // Parse input into raster pages (supports PWG/CUPS Raster, multi-page PDF, or standard images)
    let pages: Vec<RasterPageInput> =
        match parse_document_from_memory(&input_data, Some(config.quality.base_raster_dpi())) {
            Ok(p) => p,
            Err(e) => {
                eprintln!("ERROR: Failed to parse document input: {e}");
                return ExitCode::FAILURE;
            }
        };

    if pages.is_empty() {
        eprintln!("ERROR: No printable pages extracted from input");
        return ExitCode::FAILURE;
    }

    let timestamp = pixma_core::current_utc_timestamp_14();
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
