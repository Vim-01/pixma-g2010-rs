//! CUPS `#CUPS-COMMAND` maintenance and telemetry script parser and IVEC stream compiler.

use pixma_core::errors::SafetyInterlockError;
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, MaintenanceSupervisor, RollerCleanTarget,
    TestPrintType,
};
use pixma_protocol::ivec::{
    build_cleaning_cmd, build_end_job, build_get_status_cmd, build_maintenance_start_job,
    build_manual_alignment_values_cmd, build_roller_cleaning_cmd, build_test_print_cmd,
};

/// High-level CUPS maintenance or diagnostic command parsed from `#CUPS-COMMAND` files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CupsCommand {
    /// Maintenance action with optional authorization / confirmation token (e.g. for `InkFlush`).
    Maintenance(MaintenanceAction, Option<String>),
    /// Marker ink levels query.
    ReportLevels,
    /// Detailed hardware status query.
    ReportStatus,
}

/// Parses standard CUPS `#CUPS-COMMAND` files or strings into structured commands.
///
/// Supported commands (case-insensitive):
/// - `Clean` / `Clean all`: Standard printhead cleaning for all heads
/// - `CleanBlack`: Standard cleaning for Black head (PGBK / BH-4)
/// - `CleanColor`: Standard cleaning for Color head (CMY / CH-41)
/// - `DeepClean` / `DeepClean all`: Deep printhead cleaning for all heads
/// - `DeepCleanBlack`: Deep cleaning for Black head
/// - `DeepCleanColor`: Deep cleaning for Color head
/// - `InkFlush <token>`: System cleaning / CISS tube purge with confirmation token
/// - `RollerClean`: Paper feed roller cleaning
/// - `BottomPlateClean`: Platen / bottom plate rib cleaning
/// - `PrintSelfTestPage`: Nozzle check diagnostic test pattern
/// - `AutoAlign` / `HalfAutoAlign`: Semi-automatic printhead alignment pattern
/// - `ManualAlign`: Manual printhead alignment pattern
/// - `AlignmentCheck`: Head alignment verification pattern
/// - `ReportLevels`: Query marker ink levels
/// - `ReportStatus`: Query real-time hardware status
pub fn parse_cups_command_file(input: &str) -> Result<Vec<CupsCommand>, String> {
    let mut commands = Vec::new();

    for (line_no, raw_line) in input.lines().enumerate() {
        let line = raw_line.trim();

        // Skip blank lines or comments (except the header if present)
        if line.is_empty() || line == "#CUPS-COMMAND" {
            continue;
        }

        // Handle comment lines starting with '#'
        if line.starts_with('#') {
            continue;
        }

        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.is_empty() {
            continue;
        }

        let first = parts[0];
        let lower_first = first.to_ascii_lowercase();

        let cmd = match lower_first.as_str() {
            "clean" => {
                if parts.len() > 1 {
                    match parts[1].to_ascii_lowercase().as_str() {
                        "black" | "group1" => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Regular,
                                target: CleaningTarget::BlackOnly,
                            },
                            None,
                        ),
                        "color" | "group2" => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Regular,
                                target: CleaningTarget::ColorOnly,
                            },
                            None,
                        ),
                        _ => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Regular,
                                target: CleaningTarget::All,
                            },
                            None,
                        ),
                    }
                } else {
                    CupsCommand::Maintenance(
                        MaintenanceAction::Clean {
                            kind: CleaningType::Regular,
                            target: CleaningTarget::All,
                        },
                        None,
                    )
                }
            }
            "cleanblack" => CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::BlackOnly,
                },
                None,
            ),
            "cleancolor" => CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::ColorOnly,
                },
                None,
            ),
            "deepclean" => {
                if parts.len() > 1 {
                    match parts[1].to_ascii_lowercase().as_str() {
                        "black" | "group1" => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Deep,
                                target: CleaningTarget::BlackOnly,
                            },
                            None,
                        ),
                        "color" | "group2" => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Deep,
                                target: CleaningTarget::ColorOnly,
                            },
                            None,
                        ),
                        _ => CupsCommand::Maintenance(
                            MaintenanceAction::Clean {
                                kind: CleaningType::Deep,
                                target: CleaningTarget::All,
                            },
                            None,
                        ),
                    }
                } else {
                    CupsCommand::Maintenance(
                        MaintenanceAction::Clean {
                            kind: CleaningType::Deep,
                            target: CleaningTarget::All,
                        },
                        None,
                    )
                }
            }
            "deepcleanblack" => CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Deep,
                    target: CleaningTarget::BlackOnly,
                },
                None,
            ),
            "deepcleancolor" => CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Deep,
                    target: CleaningTarget::ColorOnly,
                },
                None,
            ),
            "inkflush" => {
                let token = if parts.len() > 1 {
                    Some(parts[1].to_string())
                } else {
                    None
                };
                CupsCommand::Maintenance(
                    MaintenanceAction::Clean {
                        kind: CleaningType::InkFlush,
                        target: CleaningTarget::All,
                    },
                    token,
                )
            }
            "rollerclean" => CupsCommand::Maintenance(
                MaintenanceAction::MechanicalClean(RollerCleanTarget::FeedRoller),
                None,
            ),
            "bottomplateclean" => CupsCommand::Maintenance(
                MaintenanceAction::MechanicalClean(RollerCleanTarget::BottomPlate),
                None,
            ),
            "printselftestpage" => CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::NozzleCheck),
                None,
            ),
            "autoalign" => CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::HalfAutoAlignment),
                None,
            ),
            "halfautoalign" => CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::HalfAutoAlignment),
                None,
            ),
            "manualalign" => CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::ManualAlignmentPattern),
                None,
            ),
            "alignmentcheck" => CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::AlignmentCheck),
                None,
            ),
            "reportlevels" => CupsCommand::ReportLevels,
            "reportstatus" => CupsCommand::ReportStatus,
            other => {
                return Err(format!(
                    "Unrecognized CUPS command '{other}' at line {}",
                    line_no + 1
                ));
            }
        };

        commands.push(cmd);
    }

    Ok(commands)
}

fn append_cups_command_bytes(xml_stream: &mut Vec<u8>, cmd: &CupsCommand, job_id: u32) {
    match cmd {
        CupsCommand::Maintenance(action, _token) => {
            let start_job = build_maintenance_start_job(job_id);
            xml_stream.extend_from_slice(start_job.as_bytes());
            xml_stream.resize(xml_stream.len() + start_job.len(), 0);

            match action {
                MaintenanceAction::Clean { kind, target } => {
                    xml_stream
                        .extend_from_slice(build_cleaning_cmd(job_id, *kind, *target).as_bytes());
                }
                MaintenanceAction::MechanicalClean(target) => {
                    xml_stream.extend_from_slice(
                        build_roller_cleaning_cmd(job_id, *target).as_bytes(),
                    );
                }
                MaintenanceAction::TestPrint(test_type) => {
                    xml_stream
                        .extend_from_slice(build_test_print_cmd(job_id, *test_type).as_bytes());
                }
                MaintenanceAction::ApplyManualAlignment { column_values } => {
                    xml_stream.extend_from_slice(
                        build_manual_alignment_values_cmd(job_id, column_values).as_bytes(),
                    );
                }
            }

            xml_stream.extend_from_slice(build_end_job("maintenance", job_id).as_bytes());
        }
        CupsCommand::ReportLevels | CupsCommand::ReportStatus => {
            xml_stream.extend_from_slice(build_get_status_cmd().as_bytes());
        }
    }
}

/// Marker pair emitted by `validate_and_compile_cups_commands` only after `InkFlush YES-DRAIN-INK`
/// passes `MaintenanceSupervisor::authorize_cleaning`. `cnusb-g2010` inspects this explicit boundary
/// to route confirmed tube purges through `DaemonClient::execute_maintenance` while leaving raw
/// unconfirmed `choke` XML blocked in `print_raw_stream`.
pub const CUPS_INK_FLUSH_CONFIRMED_START: &str = "<!-- PIXMA_INK_FLUSH_CONFIRMED:YES-DRAIN-INK -->";
pub const CUPS_INK_FLUSH_CONFIRMED_END: &str = "<!-- /PIXMA_INK_FLUSH_CONFIRMED -->";

/// Validates CUPS maintenance commands against hardware safety interlocks (InkFlush confirmation token,
/// cooldown timers, and waste absorber D-counter cap) and compiles them into wire-ready IVEC XML bytes.
pub fn validate_and_compile_cups_commands(
    cmds: &[CupsCommand],
    job_id: u32,
    supervisor: &mut MaintenanceSupervisor,
    now_epoch: u64,
) -> Result<Vec<u8>, SafetyInterlockError> {
    let mut xml_stream = Vec::new();
    for cmd in cmds {
        let mut confirmed_ink_flush = false;
        if let CupsCommand::Maintenance(MaintenanceAction::Clean { kind, .. }, token) = cmd {
            supervisor.authorize_cleaning(*kind, token.as_deref(), false, now_epoch)?;
            let _ = supervisor.record_cleaning_executed(*kind, now_epoch);
            confirmed_ink_flush = *kind == CleaningType::InkFlush;
        }
        if confirmed_ink_flush {
            xml_stream.extend_from_slice(CUPS_INK_FLUSH_CONFIRMED_START.as_bytes());
            xml_stream.push(b'\n');
            append_cups_command_bytes(&mut xml_stream, cmd, job_id);
            xml_stream.push(b'\n');
            xml_stream.extend_from_slice(CUPS_INK_FLUSH_CONFIRMED_END.as_bytes());
            xml_stream.push(b'\n');
        } else {
            append_cups_command_bytes(&mut xml_stream, cmd, job_id);
        }
    }
    Ok(xml_stream)
}

/// Generates wire-ready IVEC XML byte stream for executing the requested CUPS commands.
pub fn compile_cups_command_stream(cmds: &[CupsCommand], job_id: u32) -> Vec<u8> {
    let mut xml_stream = Vec::new();
    for cmd in cmds {
        append_cups_command_bytes(&mut xml_stream, cmd, job_id);
    }
    xml_stream
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_cups_command_file_comprehensive() {
        let input = r#"
#CUPS-COMMAND
Clean
Clean all
CleanBlack
CleanColor
DeepClean
DeepClean all
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

        let parsed = parse_cups_command_file(input).expect("Failed to parse command file");
        assert_eq!(parsed.len(), 18);

        // Verify some specifics
        assert_eq!(
            parsed[0],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::All,
                },
                None,
            )
        );
        assert_eq!(
            parsed[2],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::BlackOnly,
                },
                None,
            )
        );
        assert_eq!(
            parsed[3],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::ColorOnly,
                },
                None,
            )
        );
        assert_eq!(
            parsed[4],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Deep,
                    target: CleaningTarget::All,
                },
                None,
            )
        );
        assert_eq!(
            parsed[6],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Deep,
                    target: CleaningTarget::BlackOnly,
                },
                None,
            )
        );
        assert_eq!(
            parsed[8],
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::InkFlush,
                    target: CleaningTarget::All,
                },
                Some("YES-DRAIN-INK".to_string()),
            )
        );
        assert_eq!(
            parsed[9],
            CupsCommand::Maintenance(
                MaintenanceAction::MechanicalClean(RollerCleanTarget::FeedRoller),
                None,
            )
        );
        assert_eq!(
            parsed[10],
            CupsCommand::Maintenance(
                MaintenanceAction::MechanicalClean(RollerCleanTarget::BottomPlate),
                None,
            )
        );
        assert_eq!(
            parsed[11],
            CupsCommand::Maintenance(
                MaintenanceAction::TestPrint(TestPrintType::NozzleCheck),
                None,
            )
        );
        assert_eq!(parsed[16], CupsCommand::ReportLevels);
        assert_eq!(parsed[17], CupsCommand::ReportStatus);
    }

    #[test]
    fn test_compile_cups_command_stream() {
        let cmds = vec![
            CupsCommand::Maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::Regular,
                    target: CleaningTarget::All,
                },
                None,
            ),
            CupsCommand::ReportLevels,
        ];

        let xml_bytes = compile_cups_command_stream(&cmds, 42);
        let xml_str = std::str::from_utf8(&xml_bytes).unwrap();

        assert!(xml_str.contains("<ivec:operation>Cleaning</ivec:operation>"));
        assert!(xml_str.contains("<ivec:type>regular</ivec:type>"));
        assert!(xml_str.contains("<ivec:inkgroup>all</ivec:inkgroup>"));
        assert!(xml_str.contains("<ivec:jobID>00000042</ivec:jobID>"));
        assert!(xml_str.contains("<ivec:operation>GetStatus</ivec:operation>"));
    }

    #[test]
    fn test_validate_and_compile_cups_commands_enforces_interlocks() {
        use pixma_core::maintenance::{MaintenanceRecord, INK_FLUSH_CONFIRMATION_TOKEN};

        // 1. InkFlush without valid confirmation token must be blocked
        let bad_flush = parse_cups_command_file("InkFlush WRONG-TOKEN").unwrap();
        let mut sup = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        assert!(matches!(
            validate_and_compile_cups_commands(&bad_flush, 10, &mut sup, 1_000_000),
            Err(SafetyInterlockError::ConfirmationRequired { .. })
        ));

        // 2. Active cooldown from historical state must be blocked
        let reg_clean = parse_cups_command_file("CleanColor").unwrap();
        let mut sup_cooldown = MaintenanceSupervisor::in_memory(MaintenanceRecord {
            last_standard_clean_epoch: 1_000_000,
            ..Default::default()
        });
        assert!(matches!(
            validate_and_compile_cups_commands(&reg_clean, 11, &mut sup_cooldown, 1_000_010),
            Err(SafetyInterlockError::CooldownActive { .. })
        ));

        // 3. Repeated cleaning inside the same CUPS command script must trigger cooldown
        let two_cleans = parse_cups_command_file("CleanBlack\nCleanColor").unwrap();
        let mut sup_script = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        assert!(matches!(
            validate_and_compile_cups_commands(&two_cleans, 12, &mut sup_script, 2_000_000),
            Err(SafetyInterlockError::CooldownActive { .. })
        ));

        // 4. InkFlush with valid confirmation token and safe D-counter succeeds
        let ok_flush =
            parse_cups_command_file(&format!("InkFlush {INK_FLUSH_CONFIRMATION_TOKEN}")).unwrap();
        let mut sup_flush = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let bytes = validate_and_compile_cups_commands(&ok_flush, 13, &mut sup_flush, 3_000_000)
            .expect("Valid InkFlush should compile cleanly");
        assert!(std::str::from_utf8(&bytes)
            .unwrap()
            .contains("<ivec:type>choke</ivec:type>"));

        // 5. High D-counter >= 80% blocks InkFlush even with valid token
        let mut sup_full = MaintenanceSupervisor::in_memory(MaintenanceRecord {
            estimated_d_counter_percent: 80.0,
            ..Default::default()
        });
        assert!(matches!(
            validate_and_compile_cups_commands(&ok_flush, 14, &mut sup_full, 4_000_000),
            Err(SafetyInterlockError::AbsorberNearCapacity { .. })
        ));
    }
}
