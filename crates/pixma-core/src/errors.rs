//! Error codes, 7-segment LCD mappings, 4-digit Canon support codes,
//! and hardware safety violation types.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Hardware constraint violations caught before data ever reaches USB.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum HardwareConstraintViolation {
    #[error("Carriage overtravel blocked: requested {attempted_dots} dots at {dpi} DPI (hardware maximum is {max_allowed_dots} dots)")]
    CarriageOvertravel {
        attempted_dots: u32,
        max_allowed_dots: u32,
        dpi: u32,
    },
    #[error("Page height overrun blocked: requested {attempted_lines} lines at {dpi} DPI (maximum is {max_allowed_lines} lines)")]
    PageHeightExceeded {
        attempted_lines: u32,
        max_allowed_lines: u32,
        dpi: u32,
    },
    #[error("Raster buffer underrun: expected at least {expected} bytes, got {actual}")]
    BufferUnderrun { expected: usize, actual: usize },
    #[error("Unsupported hardware resolution: {0} DPI")]
    UnsupportedResolutionDpi(u32),
    #[error("Invalid bytes per pixel: {0} (expected 1, 3, or 4)")]
    InvalidBytesPerPixel(u8),
    #[error("Scan region ({x} + {width}, {y} + {height}) exceeds flatbed glass bounds ({max_w}x{max_h}) at {dpi} DPI")]
    ScanBoundsExceeded {
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        max_w: u32,
        max_h: u32,
        dpi: u32,
    },
}

/// Maintenance and thermal safety interlock errors.
#[derive(Debug, Clone, PartialEq, Error)]
pub enum SafetyInterlockError {
    #[error("Safety cooldown active for '{operation}': please wait {remaining_seconds} seconds before repeating this operation")]
    CooldownActive {
        operation: &'static str,
        remaining_seconds: u64,
    },
    #[error("Waste ink absorber hazard: D-counter is at {current_percent:.1}% (safety threshold is {max_safe_percent:.1}%). Running Ink Flush risks triggering fatal error 5B00/P07 or overflowing waste pads!")]
    AbsorberNearCapacity {
        current_percent: f32,
        max_safe_percent: f32,
    },
    #[error("Confirmation required for Ink Flush (~45 ml ink drain): must pass confirmation token '{expected_token}'")]
    ConfirmationRequired { expected_token: &'static str },
    #[error("Emergency thermal/head abort triggered by hardware support code {support_code} ({description})")]
    EmergencyHardwareAbort {
        support_code: String,
        description: String,
    },
}

/// Unified driver error type.
#[derive(Debug, Error)]
pub enum DriverError {
    #[error(transparent)]
    HardwareConstraint(#[from] HardwareConstraintViolation),
    #[error(transparent)]
    SafetyInterlock(#[from] SafetyInterlockError),
    #[error("USB transport error: {0}")]
    Transport(String),
    #[error("Protocol error: {0}")]
    Protocol(String),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Detailed diagnostic mapping for Canon G2010 7-segment LCD codes and 4-digit support codes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportCodeInfo {
    pub support_code: &'static str,
    pub lcd_code: &'static str,
    pub title: &'static str,
    pub cause: &'static str,
    pub recovery_steps: &'static str,
    pub is_thermal_or_head_hazard: bool,
    pub is_fatal_service_error: bool,
}

impl SupportCodeInfo {
    /// Looks up a 4-digit Canon support code (e.g. `"1000"`, `"5B00"`, `"5200"`) or 7-segment LCD code (`"E02"`, `"P07"`).
    pub fn lookup(code: &str) -> Option<Self> {
        let normalized = code.trim().to_ascii_uppercase();
        SUPPORT_CODE_DATABASE.iter().find_map(|entry| {
            if entry.support_code == normalized || entry.lcd_code == normalized {
                Some(entry.clone())
            } else {
                None
            }
        })
    }

    /// Returns `true` if the support code indicates a printhead or thermal fault that requires immediate job abort.
    pub fn requires_immediate_thermal_abort(code: &str) -> bool {
        Self::lookup(code)
            .map(|info| info.is_thermal_or_head_hazard || info.is_fatal_service_error)
            .unwrap_or(false)
    }
}

pub const SUPPORT_CODE_DATABASE: &[SupportCodeInfo] = &[
    SupportCodeInfo {
        support_code: "1000",
        lcd_code: "E02",
        title: "Out of Paper",
        cause: "Rear tray is empty or paper failed to feed.",
        recovery_steps: "Load paper into the rear tray and press the Black or Color button.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1300",
        lcd_code: "E03",
        title: "Paper Jam",
        cause: "Paper is jammed in the paper output slot or rear tray.",
        recovery_steps: "Gently pull the jammed paper out with both hands, reload paper, and press Black or Color.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1470",
        lcd_code: "E04",
        title: "Print Head Cartridge Not Recognized",
        cause: "FINE print head cartridge (BH-4 or CH-41) is not seated properly.",
        recovery_steps: "Open scanning unit, push down the blue ink valve lever and cartridge locking cover until they click.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1471",
        lcd_code: "E04",
        title: "Print Head Cartridge Missing",
        cause: "One or both FINE cartridges are not installed.",
        recovery_steps: "Install both Black (BH-4) and Color (CH-41) print head cartridges and latch the locking cover.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1401",
        lcd_code: "E05",
        title: "Print Head Not Recognized / Contact Fault",
        cause: "Print head temperature sensor or EEPROM contacts cannot be read.",
        recovery_steps: "Power off, remove cartridge, gently clean gold contacts with lint-free swab, and reinstall.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1403",
        lcd_code: "E05",
        title: "Print Head Temperature Sensor Error",
        cause: "Abnormal resistance detected on print head thermal diode.",
        recovery_steps: "Stop printing immediately. Allow print head to cool, check ink tubes for air, or replace print head.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1472",
        lcd_code: "E07",
        title: "Cartridge Locking Cover Unlatched",
        cause: "Print head locking cover or blue ink valve lever is open.",
        recovery_steps: "Open the top cover and firmly close both locking levers.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1700",
        lcd_code: "E08",
        title: "Waste Ink Absorber Almost Full (~95%)",
        cause: "Internal waste ink felt pads are approaching saturation.",
        recovery_steps: "Press the Black or Color button to resume printing, and schedule waste pad servicing soon.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1640",
        lcd_code: "E13",
        title: "Remaining Ink Level Cannot Be Determined",
        cause: "Firmware droplet counter has reached estimated tank threshold.",
        recovery_steps: "Visually verify all 4 CISS ink tanks are above the lower limit line, then hold Stop for 5 seconds.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "1688",
        lcd_code: "E16",
        title: "Ink Has Run Out (Firmware Estimate)",
        cause: "Firmware ink counter reached zero.",
        recovery_steps: "Refill ink tanks above the minimum line immediately! Never print with empty tanks. Hold Stop for 5+ seconds to continue.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: false,
    },
    SupportCodeInfo {
        support_code: "5100",
        lcd_code: "P02",
        title: "Carriage Jam / Optical Encoder Error",
        cause: "Carriage movement obstructed or linear timing slit strip is smudged.",
        recovery_steps: "Power off, clear any paper scraps from the carriage path, and gently wipe the clear plastic encoder strip.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: true,
    },
    SupportCodeInfo {
        support_code: "5B00",
        lcd_code: "P07",
        title: "Waste Ink Absorber Full (100%)",
        cause: "Internal D-Counter reached 100.0% capacity.",
        recovery_steps: "Physically clean or replace the bottom waste ink pads before performing a hardware Service Mode counter reset.",
        is_thermal_or_head_hazard: false,
        is_fatal_service_error: true,
    },
    SupportCodeInfo {
        support_code: "5200",
        lcd_code: "P08",
        title: "Print Head Overheating Protection Triggered",
        cause: "Print head resistor temperature exceeded safe operating threshold (often caused by air in CISS tubes).",
        recovery_steps: "Turn off the printer for at least 20 minutes. Inspect CISS ink tubes for air gaps before resuming.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: true,
    },
    SupportCodeInfo {
        support_code: "B202",
        lcd_code: "P10",
        title: "VH Monitor Error (Print Head Voltage Fault)",
        cause: "Abnormal voltage on print head power rail or shorted nozzle heater.",
        recovery_steps: "Unplug power cord, remove print heads, and inspect for ink spills on carriage contacts.",
        is_thermal_or_head_hazard: true,
        is_fatal_service_error: true,
    },
];
