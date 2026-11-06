//! CUPS backend helper functions: USB device discovery formatting and real-time
//! marker ink level telemetry reporting.

use pixma_core::models::DeviceStatus;
use pixma_transport::mock_device::G2010_IEEE1284_DEVICE_ID;
use pixma_transport::nusb_transport::discover_canon_devices;

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
    let canonical_template = format!(
        r#"direct pixma-usb://Canon/G2010%20series "Canon G2010 series" "Canon PIXMA G2010 MegaTank (Native Clean-Room)" "{G2010_IEEE1284_DEVICE_ID}" """#
    );

    // If canonical template URI is not already in discovered list, add it
    if !lines
        .iter()
        .any(|l| l.contains("pixma-usb://Canon/G2010%20series"))
    {
        lines.push(canonical_template);
    } else {
        // Still append canonical template if needed as requested: "always includes the canonical URI templates"
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
}
