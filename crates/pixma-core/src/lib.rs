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
    CANON_USB_VID, G1010_USB_PID, G2000_USB_PID, G2010_USB_PID, G2020_USB_PID, G3000_USB_PID,
    G3010_USB_PID, G4000_USB_PID, G4010_USB_PID,
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
