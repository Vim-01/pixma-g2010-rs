//! Compile-time and runtime mechanical safety types for the Canon PIXMA G2010.
//!
//! Protects the physical carriage belt, optical encoder strip, and FINE thermal
//! printheads (BH-4 Black / CH-41 Color) from out-of-range raster coordinates
//! and continuous dry-firing thermal runaway.

use crate::errors::HardwareConstraintViolation;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Discrete printer or scanner coordinate in dots at a given DPI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Dots(pub u32);

/// Physical metric coordinate in micrometers ($1\ \text{mm} = 1000\ \mu\text{m}$).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct Microns(pub u32);

impl Dots {
    pub const fn to_microns(self, dpi: u32) -> Microns {
        if dpi == 0 {
            return Microns(0);
        }
        let um = (self.0 as u64 * 25_400) / (dpi as u64);
        Microns(um as u32)
    }
}

impl Microns {
    pub const fn to_dots(self, dpi: u32) -> Dots {
        let dots = (self.0 as u64 * (dpi as u64)) / 25_400;
        Dots(dots as u32)
    }
}

/// Hard mechanical limits of the Canon PIXMA G2010 carriage and paper transport.
pub struct MechanicalBounds;

impl MechanicalBounds {
    /// Maximum carriage travel width ($8.5\text{"} = 215.9\ \text{mm}$).
    pub const MAX_WIDTH_MICRONS: Microns = Microns(215_900);
    /// Maximum single-sheet paper length ($676.0\ \text{mm}$).
    pub const MAX_LENGTH_MICRONS: Microns = Microns(676_000);
    /// Minimum custom paper width ($89.0\ \text{mm}$).
    pub const MIN_WIDTH_MICRONS: Microns = Microns(89_000);
    /// Minimum custom paper length ($127.0\ \text{mm}$).
    pub const MIN_LENGTH_MICRONS: Microns = Microns(127_000);

    /// Standard non-borderless hardware margins.
    pub const STANDARD_TOP_MARGIN_MICRONS: Microns = Microns(3_000);
    pub const STANDARD_BOTTOM_MARGIN_MICRONS: Microns = Microns(5_000);
    pub const STANDARD_SIDE_MARGIN_MICRONS: Microns = Microns(3_400);

    /// Maximum carriage dot width at 300 DPI.
    pub const MAX_CARRIAGE_DOTS_300DPI: u32 = 2_550;
    /// Maximum carriage dot width at 600 DPI.
    pub const MAX_CARRIAGE_DOTS_600DPI: u32 = 5_100;
    /// Maximum carriage dot width at 1200 DPI.
    pub const MAX_CARRIAGE_DOTS_1200DPI: u32 = 10_200;

    /// Returns the maximum permitted raster width in dots for a given horizontal DPI.
    pub const fn max_carriage_dots_for_dpi(dpi: u32) -> Result<u32, HardwareConstraintViolation> {
        match dpi {
            300 => Ok(Self::MAX_CARRIAGE_DOTS_300DPI),
            600 => Ok(Self::MAX_CARRIAGE_DOTS_600DPI),
            1200 => Ok(Self::MAX_CARRIAGE_DOTS_1200DPI),
            other => Err(HardwareConstraintViolation::UnsupportedResolutionDpi(other)),
        }
    }

    /// Returns maximum page height in dots for a given vertical DPI.
    pub const fn max_page_height_dots_for_dpi(
        dpi: u32,
    ) -> Result<u32, HardwareConstraintViolation> {
        match dpi {
            300 => Ok(7_984),
            600 => Ok(15_968),
            1200 => Ok(31_936),
            other => Err(HardwareConstraintViolation::UnsupportedResolutionDpi(other)),
        }
    }
}

/// A raster scanline whose dot width and byte buffer have been verified against
/// the physical carriage travel limits of the Canon PIXMA G2010.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedRasterLine {
    pixel_bytes: Vec<u8>,
    dot_width: u32,
    dpi: u32,
    bytes_per_pixel: u8,
}

impl ValidatedRasterLine {
    /// Validates and constructs a bounded scanline.
    ///
    /// Returns `Err(HardwareConstraintViolation)` if `dot_width` exceeds the
    /// physical carriage limit for `dpi`, or if `raw_bytes` is shorter than
    /// `dot_width * bytes_per_pixel`.
    pub fn try_new(
        raw_bytes: Vec<u8>,
        dot_width: u32,
        dpi: u32,
        bytes_per_pixel: u8,
    ) -> Result<Self, HardwareConstraintViolation> {
        let max_dots = MechanicalBounds::max_carriage_dots_for_dpi(dpi)?;
        if dot_width == 0 || dot_width > max_dots {
            return Err(HardwareConstraintViolation::CarriageOvertravel {
                attempted_dots: dot_width,
                max_allowed_dots: max_dots,
                dpi,
            });
        }
        if bytes_per_pixel != 1 && bytes_per_pixel != 3 && bytes_per_pixel != 4 {
            return Err(HardwareConstraintViolation::InvalidBytesPerPixel(
                bytes_per_pixel,
            ));
        }
        let expected_len = (dot_width as usize) * (bytes_per_pixel as usize);
        if raw_bytes.len() < expected_len {
            return Err(HardwareConstraintViolation::BufferUnderrun {
                expected: expected_len,
                actual: raw_bytes.len(),
            });
        }
        Ok(Self {
            pixel_bytes: raw_bytes[..expected_len].to_vec(),
            dot_width,
            dpi,
            bytes_per_pixel,
        })
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.pixel_bytes
    }

    pub const fn dot_width(&self) -> u32 {
        self.dot_width
    }

    pub const fn dpi(&self) -> u32 {
        self.dpi
    }

    pub const fn bytes_per_pixel(&self) -> u8 {
        self.bytes_per_pixel
    }

    /// Estimates the ink firing density (0.0..=1.0) of this line.
    /// For sRGB (`bytes_per_pixel == 3`), white `(255, 255, 255)` is 0% ink and
    /// black `(0, 0, 0)` is 100% ink. For CMYK/Grayscale, higher values or dark
    /// values are measured accordingly.
    pub fn estimate_ink_duty_cycle(&self) -> f32 {
        if self.pixel_bytes.is_empty() {
            return 0.0;
        }
        match self.bytes_per_pixel {
            3 => {
                // sRGB: ink density is proportional to (255 - channel)
                let total_ink: u64 = self
                    .pixel_bytes
                    .iter()
                    .map(|&v| (255u8.saturating_sub(v)) as u64)
                    .sum();
                let max_possible = (self.pixel_bytes.len() as u64) * 255;
                (total_ink as f32) / (max_possible as f32)
            }
            1 => {
                // SGray: 255 is white, 0 is black
                let total_ink: u64 = self
                    .pixel_bytes
                    .iter()
                    .map(|&v| (255u8.saturating_sub(v)) as u64)
                    .sum();
                let max_possible = (self.pixel_bytes.len() as u64) * 255;
                (total_ink as f32) / (max_possible as f32)
            }
            4 => {
                // CMYK: 0 is no ink, 255 is full ink
                let total_ink: u64 = self.pixel_bytes.iter().map(|&v| v as u64).sum();
                let max_possible = (self.pixel_bytes.len() as u64) * 255;
                (total_ink as f32) / (max_possible as f32)
            }
            _ => 0.0,
        }
    }
}

/// Thermal duty-cycle monitor for FINE BH-4 / CH-41 printheads.
///
/// Tracks consecutive high-density scanlines and recommends cooling pauses
/// when sustained heavy ink deposition risks overheating the thin-film heaters.
#[derive(Debug, Clone)]
pub struct DutyCycleMonitor {
    high_density_line_streak: u32,
    threshold_duty: f32,
    streak_limit_lines: u32,
}

impl Default for DutyCycleMonitor {
    fn default() -> Self {
        Self {
            high_density_line_streak: 0,
            threshold_duty: 0.85,
            streak_limit_lines: 320, // One full BH-4 printhead swath height
        }
    }
}

impl DutyCycleMonitor {
    pub fn new(threshold_duty: f32, streak_limit_lines: u32) -> Self {
        Self {
            high_density_line_streak: 0,
            threshold_duty,
            streak_limit_lines,
        }
    }

    /// Feeds a validated raster line and returns `Some(Duration)` if a thermal
    /// cooling pause is required before continuing.
    pub fn observe_line(&mut self, line: &ValidatedRasterLine) -> Option<Duration> {
        let duty = line.estimate_ink_duty_cycle();
        if duty >= self.threshold_duty {
            self.high_density_line_streak += 1;
            if self.high_density_line_streak >= self.streak_limit_lines {
                self.high_density_line_streak = 0;
                return Some(Duration::from_millis(200));
            }
        } else {
            self.high_density_line_streak = self.high_density_line_streak.saturating_sub(2);
        }
        None
    }
}

/// Typestate markers enforcing valid print job state transitions.
pub struct Uninitialized;

pub struct PageConfigured {
    pub width_dots: u32,
    pub height_dots: u32,
    pub dpi: u32,
}

pub struct StreamingRaster {
    pub width_dots: u32,
    pub total_lines: u32,
    pub emitted_lines: u32,
    pub dpi: u32,
    pub thermal_monitor: DutyCycleMonitor,
}

/// Compile-time typestate machine preventing raster lines from being emitted
/// before page geometry is validated or after page length is reached.
pub struct PrintJobTypestate<State> {
    pub state: State,
}

impl PrintJobTypestate<Uninitialized> {
    pub const fn new() -> Self {
        Self {
            state: Uninitialized,
        }
    }

    pub fn configure_page(
        self,
        width_dots: u32,
        height_dots: u32,
        dpi: u32,
    ) -> Result<PrintJobTypestate<PageConfigured>, HardwareConstraintViolation> {
        let max_w = MechanicalBounds::max_carriage_dots_for_dpi(dpi)?;
        let max_h = MechanicalBounds::max_page_height_dots_for_dpi(dpi)?;
        if width_dots == 0 || width_dots > max_w {
            return Err(HardwareConstraintViolation::CarriageOvertravel {
                attempted_dots: width_dots,
                max_allowed_dots: max_w,
                dpi,
            });
        }
        if height_dots == 0 || height_dots > max_h {
            return Err(HardwareConstraintViolation::PageHeightExceeded {
                attempted_lines: height_dots,
                max_allowed_lines: max_h,
                dpi,
            });
        }
        Ok(PrintJobTypestate {
            state: PageConfigured {
                width_dots,
                height_dots,
                dpi,
            },
        })
    }
}

impl Default for PrintJobTypestate<Uninitialized> {
    fn default() -> Self {
        Self::new()
    }
}

impl PrintJobTypestate<PageConfigured> {
    pub fn begin_streaming(self) -> PrintJobTypestate<StreamingRaster> {
        PrintJobTypestate {
            state: StreamingRaster {
                width_dots: self.state.width_dots,
                total_lines: self.state.height_dots,
                emitted_lines: 0,
                dpi: self.state.dpi,
                thermal_monitor: DutyCycleMonitor::default(),
            },
        }
    }
}

impl PrintJobTypestate<StreamingRaster> {
    /// Emits a validated raster line, checking width/DPI consistency and page length bounds.
    /// Returns an optional thermal cooling pause duration.
    pub fn accept_line(
        &mut self,
        line: &ValidatedRasterLine,
    ) -> Result<Option<Duration>, HardwareConstraintViolation> {
        if line.dpi() != self.state.dpi {
            return Err(HardwareConstraintViolation::UnsupportedResolutionDpi(
                line.dpi(),
            ));
        }
        if line.dot_width() > self.state.width_dots {
            return Err(HardwareConstraintViolation::CarriageOvertravel {
                attempted_dots: line.dot_width(),
                max_allowed_dots: self.state.width_dots,
                dpi: self.state.dpi,
            });
        }
        if self.state.emitted_lines >= self.state.total_lines {
            return Err(HardwareConstraintViolation::PageHeightExceeded {
                attempted_lines: self.state.emitted_lines + 1,
                max_allowed_lines: self.state.total_lines,
                dpi: self.state.dpi,
            });
        }
        self.state.emitted_lines += 1;
        Ok(self.state.thermal_monitor.observe_line(line))
    }
}
