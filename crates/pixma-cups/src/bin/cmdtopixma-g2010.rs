//! CUPS command filter (`cmdtopixma-g2010`) for the Canon PIXMA G2010.
//!
//! Parses `#CUPS-COMMAND` maintenance and status inquiry scripts and emits
//! native IVEC XML commands to stdout.

use pixma_cups::commands::{compile_cups_command_stream, parse_cups_command_file};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = env::args().collect();

    let job_id: u32 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(1);
    let file_arg = args.get(6).map(|s| s.as_str());

    let command_text = if let Some(path) = file_arg {
        if path != "-" && !path.is_empty() {
            match fs::read_to_string(path) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!("ERROR: Failed to read command file '{path}': {e}");
                    return ExitCode::FAILURE;
                }
            }
        } else {
            let mut s = String::new();
            if let Err(e) = io::stdin().read_to_string(&mut s) {
                eprintln!("ERROR: Failed to read commands from stdin: {e}");
                return ExitCode::FAILURE;
            }
            s
        }
    } else {
        let mut s = String::new();
        if let Err(e) = io::stdin().read_to_string(&mut s) {
            eprintln!("ERROR: Failed to read commands from stdin: {e}");
            return ExitCode::FAILURE;
        }
        s
    };

    if command_text.trim().is_empty() {
        eprintln!("ERROR: Empty command script received");
        return ExitCode::FAILURE;
    }

    let parsed_cmds = match parse_cups_command_file(&command_text) {
        Ok(cmds) => cmds,
        Err(e) => {
            eprintln!("ERROR: Failed to parse CUPS commands: {e}");
            return ExitCode::FAILURE;
        }
    };

    let xml_bytes = compile_cups_command_stream(&parsed_cmds, job_id);

    let mut stdout = io::stdout();
    if let Err(e) = stdout.write_all(&xml_bytes) {
        eprintln!("ERROR: Failed to write command XML to stdout: {e}");
        return ExitCode::FAILURE;
    }
    let _ = stdout.flush();

    ExitCode::SUCCESS
}
