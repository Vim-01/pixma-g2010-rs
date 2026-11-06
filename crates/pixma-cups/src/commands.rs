//! CUPS `#CUPS-COMMAND` maintenance and telemetry script parser and IVEC stream compiler.

use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, RollerCleanTarget, TestPrintType,
};
use pixma_protocol::bjl::BjlBuilder;
use pixma_protocol::ivec::{
    build_bjl_set_time_send_data, build_cleaning_cmd, build_end_job, build_get_status_cmd,
    build_manual_alignment_values_cmd, build_roller_cleaning_cmd, build_set_job_configuration,
    build_start_job, build_test_print_cmd,
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

/// Generates wire-ready IVEC XML byte stream for executing the requested CUPS commands.
pub fn compile_cups_command_stream(cmds: &[CupsCommand], job_id: u32) -> Vec<u8> {
    let mut xml_stream = Vec::new();

    for cmd in cmds {
        match cmd {
            CupsCommand::Maintenance(action, _token) => {
                xml_stream
                    .extend_from_slice(build_set_job_configuration(job_id, None).as_bytes());
                xml_stream.extend_from_slice(build_start_job("print", job_id).as_bytes());
                xml_stream.extend_from_slice(build_bjl_set_time_send_data(job_id).as_bytes());
                xml_stream.extend_from_slice(&BjlBuilder::time_sync_payload("20261107003000"));

                match action {
                    MaintenanceAction::Clean { kind, target } => {
                        xml_stream.extend_from_slice(
                            build_cleaning_cmd(job_id, *kind, *target).as_bytes(),
                        );
                    }
                    MaintenanceAction::MechanicalClean(target) => {
                        xml_stream.extend_from_slice(
                            build_roller_cleaning_cmd(job_id, *target).as_bytes(),
                        );
                    }
                    MaintenanceAction::TestPrint(test_type) => {
                        xml_stream.extend_from_slice(
                            build_test_print_cmd(job_id, *test_type).as_bytes(),
                        );
                    }
                    MaintenanceAction::ApplyManualAlignment { column_values } => {
                        xml_stream.extend_from_slice(
                            build_manual_alignment_values_cmd(job_id, column_values).as_bytes(),
                        );
                    }
                }

                xml_stream.extend_from_slice(build_end_job("print", job_id).as_bytes());
            }
            CupsCommand::ReportLevels | CupsCommand::ReportStatus => {
                xml_stream.extend_from_slice(build_get_status_cmd().as_bytes());
            }
        }
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
        assert!(xml_str.contains("<ivec:target>all</ivec:target>"));
        assert!(xml_str.contains("<ivec:jobID>00000042</ivec:jobID>"));
        assert!(xml_str.contains("<ivec:operation>GetStatus</ivec:operation>"));
    }
}
