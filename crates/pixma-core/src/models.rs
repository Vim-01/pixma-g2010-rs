//! USB identifiers, hardware capabilities, and live telemetry structures
//! for the Canon PIXMA G-series MegaTank devices.

use serde::{Deserialize, Serialize};

pub const CANON_USB_VID: u16 = 0x04A9;

pub const G2010_USB_PID: u16 = 0x183A;
pub const G3010_USB_PID: u16 = 0x183B;
pub const G1010_USB_PID: u16 = 0x183C;
pub const G4010_USB_PID: u16 = 0x183D;
pub const G3000_USB_PID: u16 = 0x1794;
pub const G2000_USB_PID: u16 = 0x1795;
pub const G4000_USB_PID: u16 = 0x181D;
pub const G2020_USB_PID: u16 = 0x18BD;

/// Supported Canon PIXMA G-series hardware models.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PrinterModel {
    G1010,
    G2000,
    G2010,
    G2020,
    G3000,
    G3010,
    G4000,
    G4010,
}

impl PrinterModel {
    /// Look up model by USB Product ID (assuming VID `0x04A9`).
    pub const fn from_usb_pid(pid: u16) -> Option<Self> {
        match pid {
            G1010_USB_PID => Some(Self::G1010),
            G2000_USB_PID => Some(Self::G2000),
            G2010_USB_PID => Some(Self::G2010),
            G2020_USB_PID => Some(Self::G2020),
            G3000_USB_PID => Some(Self::G3000),
            G3010_USB_PID => Some(Self::G3010),
            G4000_USB_PID => Some(Self::G4000),
            G4010_USB_PID => Some(Self::G4010),
            _ => None,
        }
    }

    pub const fn usb_pid(self) -> u16 {
        match self {
            Self::G1010 => G1010_USB_PID,
            Self::G2000 => G2000_USB_PID,
            Self::G2010 => G2010_USB_PID,
            Self::G2020 => G2020_USB_PID,
            Self::G3000 => G3000_USB_PID,
            Self::G3010 => G3010_USB_PID,
            Self::G4000 => G4000_USB_PID,
            Self::G4010 => G4010_USB_PID,
        }
    }

    pub const fn model_series_name(self) -> &'static str {
        match self {
            Self::G1010 => "G1010 series",
            Self::G2000 => "G2000 series",
            Self::G2010 => "G2010 series",
            Self::G2020 => "G2020 series",
            Self::G3000 => "G3000 series",
            Self::G3010 => "G3010 series",
            Self::G4000 => "G4000 series",
            Self::G4010 => "G4010 series",
        }
    }

    pub const fn capabilities(self) -> DeviceCapabilities {
        match self {
            Self::G1010 => DeviceCapabilities {
                has_scanner: false,
                has_adf: false,
                has_network: false,
                max_print_dpi_x: 4800,
                max_print_dpi_y: 1200,
                max_scan_dpi: 0,
                black_nozzles: 320,
                color_nozzles_per_channel: 384,
            },
            Self::G2000 | Self::G2010 | Self::G2020 => DeviceCapabilities {
                has_scanner: true,
                has_adf: false,
                has_network: false,
                max_print_dpi_x: 4800,
                max_print_dpi_y: 1200,
                max_scan_dpi: 1200,
                black_nozzles: 320,
                color_nozzles_per_channel: 384,
            },
            Self::G3000 | Self::G3010 => DeviceCapabilities {
                has_scanner: true,
                has_adf: false,
                has_network: true,
                max_print_dpi_x: 4800,
                max_print_dpi_y: 1200,
                max_scan_dpi: 1200,
                black_nozzles: 320,
                color_nozzles_per_channel: 384,
            },
            Self::G4000 | Self::G4010 => DeviceCapabilities {
                has_scanner: true,
                has_adf: true,
                has_network: true,
                max_print_dpi_x: 4800,
                max_print_dpi_y: 1200,
                max_scan_dpi: 1200,
                black_nozzles: 320,
                color_nozzles_per_channel: 384,
            },
        }
    }
}

/// Hardware capabilities of a specific Canon G-series unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCapabilities {
    pub has_scanner: bool,
    pub has_adf: bool,
    pub has_network: bool,
    pub max_print_dpi_x: u32,
    pub max_print_dpi_y: u32,
    pub max_scan_dpi: u32,
    pub black_nozzles: u32,
    pub color_nozzles_per_channel: u32,
}

/// High-level operational state reported via IVEC `<ivec:status>`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OperationalState {
    Idle,
    Printing,
    Scanning,
    Cleaning,
    Testing,
    Waiting,
    CoverOpen,
    PaperOut,
    PaperJam,
    OperatorCall,
    ServiceError,
    Offline,
}

/// FINE printhead cartridge state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CartridgeState {
    InstalledOk,
    NotRecognized,
    NotInstalled,
    LockUnlatched,
    Overheated,
}

/// Estimated CISS tank levels (0..=100%) and software warnings for GI-790 inks.
/// Note: The G2010 has no physical float/optical tank sensors; levels combine
/// firmware status flags with host-side droplet accounting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InkLevels {
    pub pigment_black_percent: u8,
    pub cyan_percent: u8,
    pub magenta_percent: u8,
    pub yellow_percent: u8,
    pub low_ink_warning: bool,
}

impl Default for InkLevels {
    fn default() -> Self {
        Self {
            pigment_black_percent: 100,
            cyan_percent: 100,
            magenta_percent: 100,
            yellow_percent: 100,
            low_ink_warning: false,
        }
    }
}

/// Complete live telemetry snapshot of the printer/scanner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DeviceStatus {
    pub model: PrinterModel,
    pub serial_number: String,
    pub firmware_version: String,
    pub state: OperationalState,
    pub status_detail: String,
    pub support_code: Option<String>,
    pub lcd_code: Option<String>,
    pub ink_levels: InkLevels,
    pub black_head_state: CartridgeState,
    pub color_head_state: CartridgeState,
    pub waste_absorber_percent: f32,
}

impl Default for DeviceStatus {
    fn default() -> Self {
        Self {
            model: PrinterModel::G2010,
            serial_number: String::from("UNKNOWN"),
            firmware_version: String::from("1.020"),
            state: OperationalState::Idle,
            status_detail: String::from("Idle"),
            support_code: None,
            lcd_code: None,
            ink_levels: InkLevels::default(),
            black_head_state: CartridgeState::InstalledOk,
            color_head_state: CartridgeState::InstalledOk,
            waste_absorber_percent: 0.0,
        }
    }
}
