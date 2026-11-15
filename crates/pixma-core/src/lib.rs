//! Core hardware models, compile-time/runtime mechanical safety bounds,
//! maintenance cooldown interlocks, and status/error telemetry for the
//! Canon PIXMA G2010 (and compatible G1010 / G2000 / G3010 / G4010) series.

pub mod errors;
pub mod maintenance;
pub mod models;
pub mod print_params;
pub mod safety;
pub mod scan_params;

pub use errors::{DriverError, HardwareConstraintViolation, SafetyInterlockError, SupportCodeInfo};
pub use maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor,
    RollerCleanTarget, TestPrintType, DEEP_CLEANING_COOLDOWN, INK_FLUSH_CONFIRMATION_TOKEN,
    INK_FLUSH_COOLDOWN, MAX_SAFE_D_COUNTER_PERCENT, STANDARD_CLEANING_COOLDOWN,
};
pub use models::{
    CartridgeState, DeviceCapabilities, DeviceStatus, InkLevels, OperationalState, PrinterModel,
    UsbHardwareMarker, CANON_USB_VID, G1010_USB_PID, G2000_USB_PID, G2010_USB_PID,
    G2020_USB_PID, G3000_USB_PID, G3010_USB_PID, G4000_USB_PID, G4010_USB_PID,
};
pub use print_params::{
    BorderlessExtension, ColorAdjustment, DuplexMode, InkDryingTime, MediaType, PageLayout,
    PaperGapMode, PaperSize, PrintJobConfig, PrintQuality, QuietMode, QuietSchedule,
};
pub use safety::{
    Dots, DutyCycleMonitor, MechanicalBounds, Microns, PageConfigured, PrintJobTypestate,
    StreamingRaster, Uninitialized, ValidatedRasterLine,
};
pub use scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};

use std::time::{SystemTime, UNIX_EPOCH};

/// Formats a Unix epoch timestamp (seconds since 1970-01-01T00:00:00Z) as a
/// 14-digit UTC timestamp string (`YYYYMMDDHHMMSS`) using Howard Hinnant's
/// proleptic Gregorian civil-date conversion.
pub fn format_utc_timestamp_14_from_secs(unix_secs: u64) -> String {
    let total_days = (unix_secs / 86_400) as i64;
    let rem_secs = (unix_secs % 86_400) as u32;
    let hour = rem_secs / 3_600;
    let minute = (rem_secs % 3_600) / 60;
    let second = rem_secs % 60;

    let z = total_days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };

    format!("{year:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}")
}

/// Returns the current UTC wall-clock time as a 14-digit `YYYYMMDDHHMMSS` string
/// for IVEC job configuration timestamps.
pub fn current_utc_timestamp_14() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format_utc_timestamp_14_from_secs(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_timestamp_14_formats_epoch_leap_and_non_leap_centuries() {
        assert_eq!(format_utc_timestamp_14_from_secs(0), "19700101000000");
        // 2000-02-29T12:34:56Z (400-year leap year)
        assert_eq!(
            format_utc_timestamp_14_from_secs(951_827_696),
            "20000229123456"
        );
        // 2024-02-29T23:59:59Z (standard leap year)
        assert_eq!(
            format_utc_timestamp_14_from_secs(1_709_251_199),
            "20240229235959"
        );
        // 2100-03-01T00:00:00Z (non-leap 100-year century)
        assert_eq!(
            format_utc_timestamp_14_from_secs(4_107_542_400),
            "21000301000000"
        );
    }

    #[test]
    fn current_utc_timestamp_14_is_14_ascii_digits() {
        let ts = current_utc_timestamp_14();
        assert_eq!(ts.len(), 14);
        assert!(ts.bytes().all(|b| b.is_ascii_digit()));
    }
}
