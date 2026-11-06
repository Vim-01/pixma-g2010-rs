//! Canon PIXMA G2010 Color Management and Ink Separation Engine.
//!
//! This crate provides:
//! - [`ColorPipeline`]: sRGB and ICC color profile transformation, followed by
//!   brightness, contrast, intensity, gamma, and CMY balance adjustments using
//!   precomputed 256-entry lookup tables for $O(N)$ real-time scanline processing.
//! - [`rgb_to_sgray_fixed_point`]: Exact fixed-point luminance conversion for fast grayscale.
//! - [`CmykPixel`]: Four-channel pixel holding Cyan, Magenta, Yellow, and Pigment Black (PGBK).
//! - [`InkSeparator`]: MegaTank GI-790 ink separation enforcing the critical hardware safety
//!   rule: Pigment Black (`k_pgbk`) is unconditionally forced to 0 on glossy/photo media to
//!   prevent nozzle clogs and coating flaking, while on plain paper dynamic GCR/UCR extracts
//!   crisp waterproof 100% PGBK text.

pub mod errors;
pub mod separation;
pub mod transform;

pub use errors::ColorError;
pub use separation::{
    CmykPixel, InkSeparator, DEFAULT_GLOSSY_MAX_TOTAL_INK, DEFAULT_PLAIN_GCR_FACTOR,
    DEFAULT_PLAIN_MAX_TOTAL_INK,
};
pub use transform::{rgb_to_sgray_fixed_point, ColorPipeline};
