//! CUPS backend helper functions: USB device discovery formatting and real-time
//! marker ink level telemetry reporting.

use crate::commands::{CUPS_INK_FLUSH_CONFIRMED_END, CUPS_INK_FLUSH_CONFIRMED_START};
use pixma_core::errors::{DriverError, SafetyInterlockError};
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, INK_FLUSH_CONFIRMATION_TOKEN,
};
use pixma_core::models::DeviceStatus;
use pixma_daemon::DaemonClient;
use pixma_transport::mock_device::G2010_IEEE1284_DEVICE_ID;
use pixma_transport::nusb_transport::discover_canon_devices;

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn contains_non_whitespace(bytes: &[u8]) -> bool {
    bytes.iter().any(|b| !b.is_ascii_whitespace())
}

fn parse_confirmed_ink_flush_target(span: &[u8]) -> Result<CleaningTarget, DriverError> {
    const MAX_CONFIRMED_SPAN_BYTES: usize = 16 * 1024;
    let reject = || {
        DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired {
            expected_token: INK_FLUSH_CONFIRMATION_TOKEN,
        })
    };

    if span.len() > MAX_CONFIRMED_SPAN_BYTES {
        return Err(reject());
    }
    let text = std::str::from_utf8(span);
    // Notice StartJob/EndJob around the cleaning XML contains NUL padding bytes (`0x00`),
    // which are valid UTF-8, so handle UTF-8 cleanly and ensure exactly one `choke` operation.
    let Ok(xml) = text else {
        return Err(reject());
    };
    let clean_ops = xml.matches("<ivec:operation>Cleaning</ivec:operation>").count();
    let choke_ops = xml.matches("<ivec:type>choke</ivec:type>").count();
    let cmd_tags = xml.matches("<cmd").count();
    if clean_ops != 1 || choke_ops != 1 || cmd_tags > 3 {
        return Err(reject());
    }

    if xml.contains("<ivec:inkgroup>group1</ivec:inkgroup>") {
        Ok(CleaningTarget::BlackOnly)
    } else if xml.contains("<ivec:inkgroup>group2</ivec:inkgroup>") {
        Ok(CleaningTarget::ColorOnly)
    } else {
        Ok(CleaningTarget::All)
    }
}

/// Dispatches CUPS backend payload bytes to the daemon/device.
///
/// Ordinary print/raster jobs and standard cleaning streams are sent via `print_raw_stream`.
/// Confirmed `InkFlush YES-DRAIN-INK` blocks emitted by `cmdtopixma-g2010` are routed through
/// `DaemonClient::execute_maintenance` with the explicit confirmation token, while unconfirmed
/// raw `choke` XML still hits `print_raw_stream` and is rejected fail-closed.
pub async fn dispatch_cups_payload(
    client: &mut DaemonClient,
    print_data: &[u8],
) -> Result<(), DriverError> {
    let start_marker = CUPS_INK_FLUSH_CONFIRMED_START.as_bytes();
    let end_marker = CUPS_INK_FLUSH_CONFIRMED_END.as_bytes();

    if find_subslice(print_data, start_marker).is_none() {
        return client.print_raw_stream(print_data).await;
    }

    let mut offset = 0usize;
    while let Some(rel_start) = find_subslice(&print_data[offset..], start_marker) {
        let prefix_end = offset + rel_start;
        let prefix = &print_data[offset..prefix_end];
        if contains_non_whitespace(prefix) {
            client.print_raw_stream(prefix).await?;
        }

        let span_start = prefix_end + start_marker.len();
        let Some(rel_end) = find_subslice(&print_data[span_start..], end_marker) else {
            return Err(DriverError::SafetyInterlock(
                SafetyInterlockError::ConfirmationRequired {
                    expected_token: INK_FLUSH_CONFIRMATION_TOKEN,
                },
            ));
        };
        let span = &print_data[span_start..span_start + rel_end];
        let target = parse_confirmed_ink_flush_target(span)?;
        client
            .execute_maintenance(
                MaintenanceAction::Clean {
                    kind: CleaningType::InkFlush,
                    target,
                },
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
            )
            .await?;

        offset = span_start + rel_end + end_marker.len();
    }

    let tail = &print_data[offset..];
    if contains_non_whitespace(tail) {
        client.print_raw_stream(tail).await?;
    }

    Ok(())
}

/// Formats CUPS backend device discovery lines conforming to CUPS backend discovery specifications:
/// `direct <device-uri> "<make-and-model>" "<info/description>" "<1284-device-id>" "<location>"`.
///
/// Discovers attached Canon G-series USB devices via `nusb` and always guarantees inclusion
/// of the canonical URI template for `pixma-usb://Canon/G2010%20series`.
pub fn format_cups_discovery_lines() -> Vec<String> {
    let mut lines = Vec::new();

    // 1. Enumerate physical USB devices
    if let Ok(devices) = discover_canon_devices() {
        for (model, serial) in devices {
            let model_name = model.model_series_name();
            let encoded_model = model_name.replace(' ', "%20");
            let uri = format!("pixma-usb://Canon/{encoded_model}?serial={serial}");
            let make_model = format!("Canon {model_name}");
            let info = format!("Canon PIXMA {model_name} MegaTank");
            let device_id = G2010_IEEE1284_DEVICE_ID;

            lines.push(format!(
                r#"direct {uri} "{make_model}" "{info}" "{device_id}" """#
            ));
        }
    }

    // 2. Canonical template entry for G2010
    let canonical_prefix = "direct pixma-usb://Canon/G2010%20series ";
    let canonical_template = format!(
        r#"direct pixma-usb://Canon/G2010%20series "Canon G2010 series" "Canon PIXMA G2010 MegaTank (Native Clean-Room)" "{G2010_IEEE1284_DEVICE_ID}" """#
    );

    if !lines.iter().any(|l| l.starts_with(canonical_prefix)) {
        lines.push(canonical_template);
    }

    lines
}

/// Formats CUPS `ATTR:` lines for marker ink names, colors, types, and current fill levels
/// to be emitted to CUPS `stderr`.
///
/// CUPS parses these lines from backend `stderr` to update printer ink level meters:
/// - `ATTR: marker-names="Pigment Black (GI-790)","Cyan (GI-790)","Magenta (GI-790)","Yellow (GI-790)"`
/// - `ATTR: marker-colors="#000000","#00FFFF","#FF00FF","#FFFF00"`
/// - `ATTR: marker-types=ink-cartridge,ink-cartridge,ink-cartridge,ink-cartridge`
/// - `ATTR: marker-levels=100,100,100,100`
pub fn format_cups_marker_attributes(status: &DeviceStatus) -> Vec<String> {
    vec![
        "ATTR: marker-names=\"Pigment Black (GI-790)\",\"Cyan (GI-790)\",\"Magenta (GI-790)\",\"Yellow (GI-790)\"".to_string(),
        "ATTR: marker-colors=\"#000000\",\"#00FFFF\",\"#FF00FF\",\"#FFFF00\"".to_string(),
        "ATTR: marker-types=ink-cartridge,ink-cartridge,ink-cartridge,ink-cartridge".to_string(),
        format!(
            "ATTR: marker-levels={},{},{},{}",
            status.ink_levels.pigment_black_percent,
            status.ink_levels.cyan_percent,
            status.ink_levels.magenta_percent,
            status.ink_levels.yellow_percent,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::models::InkLevels;

    #[test]
    fn test_format_cups_discovery_lines_includes_canonical() {
        let lines = format_cups_discovery_lines();
        assert!(!lines.is_empty());
        assert!(lines
            .iter()
            .any(|l| l.contains("pixma-usb://Canon/G2010%20series")));
        assert!(lines.iter().any(|l| l.starts_with("direct pixma-usb://")));
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.starts_with("direct pixma-usb://Canon/G2010%20series "))
                .count(),
            1
        );
    }

    #[test]
    fn test_format_cups_marker_attributes() {
        let status = DeviceStatus {
            ink_levels: InkLevels {
                pigment_black_percent: 85,
                cyan_percent: 70,
                magenta_percent: 65,
                yellow_percent: 90,
                low_ink_warning: false,
            },
            ..Default::default()
        };

        let attrs = format_cups_marker_attributes(&status);
        assert_eq!(attrs.len(), 4);
        assert_eq!(
            attrs[0],
            "ATTR: marker-names=\"Pigment Black (GI-790)\",\"Cyan (GI-790)\",\"Magenta (GI-790)\",\"Yellow (GI-790)\""
        );
        assert_eq!(
            attrs[1],
            "ATTR: marker-colors=\"#000000\",\"#00FFFF\",\"#FF00FF\",\"#FFFF00\""
        );
        assert_eq!(
            attrs[2],
            "ATTR: marker-types=ink-cartridge,ink-cartridge,ink-cartridge,ink-cartridge"
        );
        assert_eq!(attrs[3], "ATTR: marker-levels=85,70,65,90");
    }

    #[tokio::test]
    async fn dispatch_cups_payload_executes_confirmed_ink_flush_and_enforces_cooldown() {
        use crate::commands::{parse_cups_command_file, validate_and_compile_cups_commands};
        use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};

        let dir = std::env::temp_dir().join(format!(
            "pixma-cups-flush-test-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        let state_path = dir.join("state.json");

        let cmds = parse_cups_command_file(&format!(
            "InkFlush {INK_FLUSH_CONFIRMATION_TOKEN}\nReportStatus"
        ))
        .unwrap();
        let mut sup = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let payload = validate_and_compile_cups_commands(&cmds, 77, &mut sup, 1_000_000).unwrap();

        let mut client = DaemonClient::connect_mock(Some(&state_path)).unwrap();
        dispatch_cups_payload(&mut client, &payload)
            .await
            .expect("Confirmed CUPS InkFlush YES-DRAIN-INK must succeed end-to-end");

        let persisted = MaintenanceSupervisor::load_readonly(&state_path).unwrap();
        assert_eq!(persisted.record().total_ink_flushes, 1);
        assert!(persisted.record().estimated_d_counter_percent >= 20.0);
        assert!(persisted.record().last_ink_flush_epoch > 0);

        // Second dispatch hits the 24h InkFlush cooldown recorded on the daemon controller
        let second_err = dispatch_cups_payload(&mut client, &payload)
            .await
            .expect_err("Repeated confirmed InkFlush inside 24h must fail cooldown");
        assert!(matches!(
            second_err,
            DriverError::SafetyInterlock(SafetyInterlockError::CooldownActive { .. })
        ));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dispatch_cups_payload_rejects_unconfirmed_choke_and_malformed_marker() {
        use pixma_protocol::ivec::build_cleaning_cmd;

        let mut client = DaemonClient::connect_mock(None).unwrap();
        let raw_choke = build_cleaning_cmd(88, CleaningType::InkFlush, CleaningTarget::All);
        let err = dispatch_cups_payload(&mut client, raw_choke.as_bytes())
            .await
            .expect_err("Unconfirmed raw choke XML must still fail closed");
        assert!(matches!(
            err,
            DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired { .. })
        ));

        let unclosed_marker = format!("{CUPS_INK_FLUSH_CONFIRMED_START}\n{raw_choke}");
        let err_marker = dispatch_cups_payload(&mut client, unclosed_marker.as_bytes())
            .await
            .expect_err("Unclosed marker must fail closed");
        assert!(matches!(
            err_marker,
            DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired { .. })
        ));
    }
}
