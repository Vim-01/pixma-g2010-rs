//! ASCII status gauge rendering and diagnostic support code presentation.

use pixma_core::errors::SupportCodeInfo;
use pixma_core::models::{CartridgeState, DeviceStatus, OperationalState};

/// Renders a 20-character horizontal progress bar gauge.
pub fn render_progress_bar(percent: f32, width: usize) -> String {
    let clamped = percent.clamp(0.0, 100.0);
    let filled = ((clamped / 100.0) * width as f32).round() as usize;
    let unfilled = width.saturating_sub(filled);
    format!("[{}{}]", "█".repeat(filled), "░".repeat(unfilled))
}

/// Formats the FINE printhead cartridge state.
pub fn format_cartridge_state(state: CartridgeState) -> &'static str {
    match state {
        CartridgeState::InstalledOk => "OK (Installed)",
        CartridgeState::NotRecognized => "ERROR: Not Recognized",
        CartridgeState::NotInstalled => "MISSING: Not Installed",
        CartridgeState::LockUnlatched => "WARNING: Lever Unlatched",
        CartridgeState::Overheated => "CRITICAL: Overheated",
    }
}

/// Formats high-level operational state.
pub fn format_operational_state(state: OperationalState) -> &'static str {
    match state {
        OperationalState::Idle => "Idle / Ready",
        OperationalState::Printing => "Printing",
        OperationalState::Scanning => "Scanning",
        OperationalState::Cleaning => "Cleaning Print Head",
        OperationalState::Testing => "Printing Test Pattern",
        OperationalState::Waiting => "Waiting",
        OperationalState::CoverOpen => "Cover Open",
        OperationalState::PaperOut => "Out of Paper",
        OperationalState::PaperJam => "Paper Jam",
        OperationalState::OperatorCall => "Operator Call Required",
        OperationalState::ServiceError => "Service Error",
        OperationalState::Offline => "Offline",
    }
}

/// Renders a full ASCII telemetry report for the Canon PIXMA G2010.
pub fn render_ascii_status(status: &DeviceStatus) -> String {
    let mut out = String::new();

    out.push_str(
        "================================================================================\n",
    );
    out.push_str(&format!(
        " Canon PIXMA {} Telemetry Report\n",
        status.model.model_series_name()
    ));
    out.push_str(
        "================================================================================\n",
    );

    out.push_str(&format!(
        " Serial Number : {}\n",
        if status.serial_number.is_empty() {
            "UNKNOWN"
        } else {
            &status.serial_number
        }
    ));
    out.push_str(&format!(" Firmware      : {}\n", status.firmware_version));
    out.push_str(&format!(
        " Device State  : {} ({})\n",
        format_operational_state(status.state),
        status.status_detail
    ));
    out.push_str(
        "--------------------------------------------------------------------------------\n",
    );

    // Ink Tanks
    out.push_str(" GI-790 MegaTank CISS Ink Levels (Estimated by Droplet Counter):\n");
    out.push_str(
        "   * Note: MegaTank tanks have no electronic float sensors. Check windows on front.\n",
    );
    let pgbk = status.ink_levels.pigment_black_percent;
    let cyan = status.ink_levels.cyan_percent;
    let mag = status.ink_levels.magenta_percent;
    let yel = status.ink_levels.yellow_percent;

    out.push_str(&format!(
        "   Pigment Black (PGBK) {} {:3}%\n",
        render_progress_bar(pgbk as f32, 20),
        pgbk
    ));
    out.push_str(&format!(
        "   Cyan          (C)    {} {:3}%\n",
        render_progress_bar(cyan as f32, 20),
        cyan
    ));
    out.push_str(&format!(
        "   Magenta       (M)    {} {:3}%\n",
        render_progress_bar(mag as f32, 20),
        mag
    ));
    out.push_str(&format!(
        "   Yellow        (Y)    {} {:3}%\n",
        render_progress_bar(yel as f32, 20),
        yel
    ));

    if status.ink_levels.low_ink_warning {
        out.push_str("   [!] Low Ink Warning: Inspect ink level marks on physical tanks.\n");
    }
    out.push_str(
        "--------------------------------------------------------------------------------\n",
    );

    // Waste Ink Absorber
    out.push_str(" Waste Ink Absorber (D-Counter):\n");
    let absorber = status.waste_absorber_percent;
    out.push_str(&format!(
        "   Saturation   {} {:5.1}% / 80.0% safety block (100% fatal 5B00/P07)\n",
        render_progress_bar(absorber, 20),
        absorber
    ));
    if absorber >= 80.0 {
        out.push_str(
            "   [!] DANGER: Waste ink absorber near maximum capacity. Ink Flush blocked!\n",
        );
    }
    out.push_str(
        "--------------------------------------------------------------------------------\n",
    );

    // FINE Printheads
    out.push_str(" FINE Printhead Status:\n");
    out.push_str(&format!(
        "   Black Head (BH-4) : {}\n",
        format_cartridge_state(status.black_head_state)
    ));
    out.push_str(&format!(
        "   Color Head (CH-41): {}\n",
        format_cartridge_state(status.color_head_state)
    ));
    out.push_str(
        "--------------------------------------------------------------------------------\n",
    );

    // Diagnostics & Support Codes
    out.push_str(" Diagnostic / Error Status:\n");
    if let Some(ref code) = status.support_code {
        let lcd = status.lcd_code.as_deref().unwrap_or("---");
        if let Some(info) = SupportCodeInfo::lookup(code) {
            out.push_str(&format!(
                "   [ALERT] Support Code: {} (LCD: {}) - {}\n",
                info.support_code, info.lcd_code, info.title
            ));
            out.push_str(&format!("   Cause    : {}\n", info.cause));
            out.push_str(&format!("   Recovery : {}\n", info.recovery_steps));
            if info.is_thermal_or_head_hazard {
                out.push_str(
                    "   Hazard   : Thermal / Printhead Hazard (Stop printing immediately!)\n",
                );
            }
            if info.is_fatal_service_error {
                out.push_str("   Severity : Fatal Service Error (Requires servicing)\n");
            }
        } else {
            out.push_str(&format!(
                "   [ALERT] Support Code: {} (LCD: {})\n",
                code, lcd
            ));
            out.push_str("   Check Canon G2010 online manual for diagnostic steps.\n");
        }
    } else {
        out.push_str("   Status: Normal / OK. No active hardware support codes.\n");
    }

    out.push_str(
        "================================================================================\n",
    );
    out
}
