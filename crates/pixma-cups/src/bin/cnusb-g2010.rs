//! CUPS USB backend (`cnusb-g2010`) for the Canon PIXMA G2010.
//!
//! Handles CUPS device discovery mode (when invoked with 0 arguments) and
//! active job execution (reporting marker telemetry to stderr and streaming
//! print job bytes via `DaemonClient`).

use pixma_cups::backend::{format_cups_discovery_lines, format_cups_marker_attributes};
use pixma_daemon::DaemonClient;
use std::env;
use std::fs::File;
use std::io::{self, Read};
use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    // 1. CUPS Discovery Mode: Invoked with no arguments
    if args.len() == 1 {
        let lines = format_cups_discovery_lines();
        for line in lines {
            println!("{line}");
        }
        return ExitCode::SUCCESS;
    }

    // 2. Active Job Mode: Invoked with standard CUPS backend arguments
    // argv[1] = job-id
    // argv[2] = user
    // argv[3] = title
    // argv[4] = copies
    // argv[5] = options
    // argv[6] = print file (optional; stdin if omitted)
    let file_arg = args.get(6).map(|s| s.as_str());

    let mut print_data = Vec::new();
    if let Some(path) = file_arg {
        if path != "-" && !path.is_empty() {
            match File::open(path) {
                Ok(mut f) => {
                    if let Err(e) = f.read_to_end(&mut print_data) {
                        eprintln!("ERROR: Failed to read print file '{path}': {e}");
                        return ExitCode::FAILURE;
                    }
                }
                Err(e) => {
                    eprintln!("ERROR: Could not open print file '{path}': {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else if let Err(e) = io::stdin().read_to_end(&mut print_data) {
            eprintln!("ERROR: Failed to read print stream from stdin: {e}");
            return ExitCode::FAILURE;
        }
    } else if let Err(e) = io::stdin().read_to_end(&mut print_data) {
        eprintln!("ERROR: Failed to read print stream from stdin: {e}");
        return ExitCode::FAILURE;
    }

    if print_data.is_empty() {
        eprintln!("ERROR: Empty print data payload");
        return ExitCode::FAILURE;
    }

    // Connect to running pixmad or fallback to local controller
    let mut client = match DaemonClient::connect_or_fallback(None, None).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("ERROR: Failed to establish printer transport connection: {e}");
            return ExitCode::FAILURE;
        }
    };

    // Query printer status and report marker levels to CUPS stderr
    if let Ok(status) = client.get_status().await {
        for attr in format_cups_marker_attributes(&status) {
            eprintln!("{attr}");
        }
    }

    // Stream raw print job bytes to the printer bulk pipe
    if let Err(e) = client.print_raw_stream(&print_data).await {
        eprintln!("ERROR: Failed to stream print job to device: {e}");
        return ExitCode::FAILURE;
    }

    // Query final status and report updated marker levels
    if let Ok(status) = client.get_status().await {
        for attr in format_cups_marker_attributes(&status) {
            eprintln!("{attr}");
        }
    }

    ExitCode::SUCCESS
}
