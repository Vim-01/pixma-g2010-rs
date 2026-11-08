//! Scanner parameters and flatbed geometry validation for the Canon PIXMA G2010.

use crate::errors::HardwareConstraintViolation;
use serde::{Deserialize, Serialize};

/// Supported optical and interpolated scan resolutions on the G2010 CIS flatbed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum ScanResolution {
    Dpi75 = 75,
    Dpi100 = 100,
    Dpi150 = 150,
    Dpi200 = 200,
    Dpi300 = 300,
    Dpi400 = 400,
    Dpi600 = 600,
    Dpi1200 = 1200,
}

impl ScanResolution {
    pub const fn as_dpi(self) -> u16 {
        self as u16
    }

    pub const fn from_dpi(dpi: u32) -> Result<Self, HardwareConstraintViolation> {
        match dpi {
            75 => Ok(Self::Dpi75),
            100 => Ok(Self::Dpi100),
            150 => Ok(Self::Dpi150),
            200 => Ok(Self::Dpi200),
            300 => Ok(Self::Dpi300),
            400 => Ok(Self::Dpi400),
            600 => Ok(Self::Dpi600),
            1200 => Ok(Self::Dpi1200),
            other => Err(HardwareConstraintViolation::UnsupportedResolutionDpi(other)),
        }
    }

    /// Returns the CIS sensor vertical line-stagger offsets `(green_delay_lines, blue_delay_lines)`
    /// relative to the Red channel for this resolution.
    pub const fn cis_line_stagger_offsets(self) -> (usize, usize) {
        match self {
            Self::Dpi75
            | Self::Dpi100
            | Self::Dpi150
            | Self::Dpi200
            | Self::Dpi300
            | Self::Dpi400 => (0, 0),
            Self::Dpi600 => (2, 4),
            Self::Dpi1200 => (4, 8),
        }
    }

    /// Maximum flatbed scan dimensions `(max_width_px, max_height_px)` at this resolution
    /// (638 x 877 pixels at 75 DPI base platen geometry for Canon PIXMA G2010 `04a9:183a`).
    pub const fn max_flatbed_pixels(self) -> (u32, u32) {
        let dpi = self.as_dpi() as u32;
        let max_w = (dpi * 638) / 75;
        let max_h = (dpi * 877) / 75;
        (max_w, max_h)
    }

    /// Exact ISO A4 flatbed scan dimensions `(width_px, height_px)` at this resolution
    /// (210.0 mm x 297.0 mm = 620 x 877 pixels at 75 DPI), excluding the 6 mm right-hand
    /// Letter platen overhang (scanner lid bezel).
    pub const fn a4_flatbed_pixels(self) -> (u32, u32) {
        let dpi = self.as_dpi() as u32;
        let a4_w = (dpi * 620) / 75;
        let a4_h = (dpi * 877) / 75;
        (a4_w, a4_h)
    }
}

/// Scanner color acquisition modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScanColorMode {
    /// 24-bit RGB Color (8 bits per channel).
    Color24Bit,
    /// 8-bit Grayscale.
    Grayscale8Bit,
    /// 1-bit Black & White Lineart (software adaptive threshold).
    Lineart1Bit,
}

impl ScanColorMode {
    pub const fn mp2_mode_bytes(self) -> [u8; 4] {
        match self {
            Self::Color24Bit => [0x02, 0x01, 0x03, 0x01],
            Self::Grayscale8Bit => [0x02, 0x01, 0x01, 0x01],
            Self::Lineart1Bit => [0x01, 0x01, 0x01, 0x00],
        }
    }

    pub const fn depth_byte(self) -> u8 {
        match self {
            Self::Color24Bit => 0x08,
            Self::Grayscale8Bit | Self::Lineart1Bit => 0x04,
        }
    }

    pub const fn bits_per_pixel_byte(self) -> u8 {
        match self {
            Self::Color24Bit => 0x18,
            Self::Grayscale8Bit | Self::Lineart1Bit => 0x08,
        }
    }

    pub const fn bytes_per_pixel(self) -> usize {
        match self {
            Self::Color24Bit => 3,
            Self::Grayscale8Bit | Self::Lineart1Bit => 1,
        }
    }
}

/// Validated flatbed scan bounding box in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScanGeometry {
    pub x_offset: u32,
    pub y_offset: u32,
    pub width: u32,
    pub height: u32,
}

impl ScanGeometry {
    pub fn full_platen(resolution: ScanResolution) -> Self {
        let (width, height) = resolution.max_flatbed_pixels();
        Self {
            x_offset: 0,
            y_offset: 0,
            width,
            height,
        }
    }

    pub fn a4(resolution: ScanResolution) -> Self {
        let (width, height) = resolution.a4_flatbed_pixels();
        Self {
            x_offset: 0,
            y_offset: 0,
            width,
            height,
        }
    }

    pub fn validate(self, resolution: ScanResolution) -> Result<Self, HardwareConstraintViolation> {
        let (max_w, max_h) = resolution.max_flatbed_pixels();
        if self.width == 0
            || self.height == 0
            || self.x_offset.saturating_add(self.width) > max_w
            || self.y_offset.saturating_add(self.height) > max_h
        {
            return Err(HardwareConstraintViolation::ScanBoundsExceeded {
                x: self.x_offset,
                y: self.y_offset,
                width: self.width,
                height: self.height,
                max_w,
                max_h,
                dpi: resolution.as_dpi() as u32,
            });
        }
        Ok(self)
    }
}

/// Complete scan job parameters including ScanGear Pro post-processing flags.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ScanParameters {
    pub resolution: ScanResolution,
    pub color_mode: ScanColorMode,
    pub geometry: ScanGeometry,
    pub auto_multi_crop: bool,
    pub deskew: bool,
    pub descreen_moire: bool,
    pub unsharp_mask: bool,
    pub remove_dust_scratches: bool,
    pub correct_gutter_shadow: bool,
    pub gamma: f32,
}

impl Default for ScanParameters {
    fn default() -> Self {
        let resolution = ScanResolution::Dpi300;
        Self {
            resolution,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry::full_platen(resolution),
            auto_multi_crop: false,
            deskew: false,
            descreen_moire: false,
            unsharp_mask: true,
            remove_dust_scratches: false,
            correct_gutter_shadow: false,
            gamma: 2.2,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::SupportCodeInfo;
    use crate::maintenance::{
        CleaningType, MaintenanceRecord, MaintenanceSupervisor, INK_FLUSH_CONFIRMATION_TOKEN,
    };
    use crate::safety::{PrintJobTypestate, ValidatedRasterLine};

    #[test]
    fn rejects_carriage_overtravel() {
        let bad = ValidatedRasterLine::try_new(vec![0u8; 6000 * 3], 5101, 600, 3);
        assert!(bad.is_err());

        let good = ValidatedRasterLine::try_new(vec![255u8; 5100 * 3], 5100, 600, 3);
        assert!(good.is_ok());
    }

    #[test]
    fn enforces_ink_flush_cooldown_and_d_counter_limit() {
        let mut sup = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        // Missing token must fail
        assert!(sup
            .authorize_cleaning(CleaningType::InkFlush, None, false, 1000)
            .is_err());
        // With token succeeds first time
        assert!(sup
            .authorize_cleaning(
                CleaningType::InkFlush,
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
                1000
            )
            .is_ok());
        sup.record_cleaning_executed(CleaningType::InkFlush, 1000)
            .unwrap();
        // Immediate second flush within 24h must fail
        assert!(sup
            .authorize_cleaning(
                CleaningType::InkFlush,
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
                2000
            )
            .is_err());
        // After 24h + 1s succeeds
        assert!(sup
            .authorize_cleaning(
                CleaningType::InkFlush,
                Some(INK_FLUSH_CONFIRMATION_TOKEN),
                false,
                1000 + 86401
            )
            .is_ok());
    }

    #[test]
    fn typestate_prevents_page_overrun() {
        let job = PrintJobTypestate::new()
            .configure_page(100, 2, 600)
            .unwrap();
        let mut stream = job.begin_streaming();
        let line = ValidatedRasterLine::try_new(vec![0u8; 300], 100, 600, 3).unwrap();
        assert!(stream.accept_line(&line).is_ok());
        assert!(stream.accept_line(&line).is_ok());
        assert!(stream.accept_line(&line).is_err());
    }

    #[test]
    fn support_code_lookup_identifies_thermal_hazards() {
        assert!(SupportCodeInfo::requires_immediate_thermal_abort("5200"));
        assert!(SupportCodeInfo::requires_immediate_thermal_abort("P08"));
        assert!(!SupportCodeInfo::requires_immediate_thermal_abort("1000"));
    }

    #[test]
    fn validates_flatbed_scan_geometry_and_cis_stagger() {
        let geom = ScanGeometry::full_platen(ScanResolution::Dpi600);
        assert!(geom.validate(ScanResolution::Dpi600).is_ok());
        assert_eq!(ScanResolution::Dpi600.cis_line_stagger_offsets(), (2, 4));
        assert_eq!(ScanResolution::Dpi1200.cis_line_stagger_offsets(), (4, 8));

        let out_of_bounds = ScanGeometry {
            x_offset: 100,
            y_offset: 0,
            width: geom.width,
            height: geom.height,
        };
        assert!(out_of_bounds.validate(ScanResolution::Dpi600).is_err());
    }
}
