//! CUPS command filter (`cmdtopixma-g2010`) for the Canon PIXMA G2010.
//!
//! Parses `#CUPS-COMMAND` maintenance and status inquiry scripts and emits
//! native IVEC XML commands to stdout.

use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_cups::commands::{parse_cups_command_file, validate_and_compile_cups_commands};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::ExitCode;

fn load_readonly_cups_supervisor_from_candidates(
    candidates: &[PathBuf],
) -> Result<MaintenanceSupervisor, io::Error> {
    for path in candidates {
        match MaintenanceSupervisor::load_readonly(path) {
            Ok(supervisor) => return Ok(supervisor),
            Err(e) if e.kind() == io::ErrorKind::InvalidData => return Err(e),
            Err(_) => continue,
        }
    }

    eprintln!(
        "WARNING: Maintenance state file not found or unreadable; using in-memory interlocks without historical cooldown telemetry"
    );
    Ok(MaintenanceSupervisor::in_memory(MaintenanceRecord::default()))
}

fn load_readonly_cups_supervisor() -> Result<MaintenanceSupervisor, io::Error> {
    let mut candidates = Vec::new();
    if let Some(p) = env::var_os("PIXMA_STATE_FILE").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(p));
    }
    candidates.push(PathBuf::from("/var/lib/pixma-g2010/state.json"));
    candidates.push(PathBuf::from("/var/lib/pixma/maintenance_state.json"));
    if let Some(xdg) = env::var_os("XDG_STATE_HOME").filter(|v| !v.is_empty()) {
        candidates.push(PathBuf::from(xdg).join("pixma-g2010").join("state.json"));
    }
    if let Some(home) = env::var_os("HOME").filter(|v| !v.is_empty()) {
        candidates.push(
            PathBuf::from(home)
                .join(".local")
                .join("state")
                .join("pixma-g2010")
                .join("state.json"),
        );
    }

    load_readonly_cups_supervisor_from_candidates(&candidates)
}

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

    let mut supervisor = match load_readonly_cups_supervisor() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("ERROR: Corrupted maintenance state file rejected: {e}");
            return ExitCode::FAILURE;
        }
    };
    let now_epoch = MaintenanceSupervisor::current_unix_epoch();
    let xml_bytes = match validate_and_compile_cups_commands(
        &parsed_cmds,
        job_id,
        &mut supervisor,
        now_epoch,
    ) {
        Ok(bytes) => bytes,
        Err(e) => {
            eprintln!("ERROR: Maintenance safety interlock rejected CUPS command: {e}");
            return ExitCode::FAILURE;
        }
    };

    let mut stdout = io::stdout();
    if let Err(e) = stdout.write_all(&xml_bytes) {
        eprintln!("ERROR: Failed to write command XML to stdout: {e}");
        return ExitCode::FAILURE;
    }
    let _ = stdout.flush();

    ExitCode::SUCCESS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_readonly_cups_supervisor_rejects_corrupted_json() {
        let dir = env::temp_dir().join(format!(
            "pixma-cmdtopixma-corrupt-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();

        let missing = dir.join("missing.json");
        let corrupt = dir.join("corrupt.json");
        let fallback_valid = dir.join("valid.json");
        fs::write(&corrupt, b"{bad json").unwrap();
        fs::write(&fallback_valid, "{}").unwrap();

        let err = load_readonly_cups_supervisor_from_candidates(&[
            missing,
            corrupt,
            fallback_valid,
        ])
        .expect_err("Corrupted candidate state file must fail instead of falling through to zeroed state");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);

        let _ = fs::remove_dir_all(&dir);
    }
}
