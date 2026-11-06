//! Pure-Rust scanner post-processing engine for the Canon PIXMA G2010.
//!
//! Provides Clean-Room implementations of:
//! - Contact Image Sensor (CIS) photodiode line-stagger vertical realignment (`LineStaggerBuffer`).
//! - ScanGear Pro image restoration filters:
//!   - Edge sharpening via Gaussian unsharp masking (`unsharp_mask`).
//!   - Halftone moiré descreening via 2D Fast Fourier Transform on luminance (`descreen_moire_2d_fft`).
//!   - Glass dust and scratch removal using morphological median outlier filtering (`remove_dust_and_scratches`).
//!   - Book-spine gutter shadow illumination compensation (`correct_gutter_shadow`).
//! - Flatbed geometry:
//!   - Automatic multi-document segmentation and cropping (`auto_multi_crop`).
//!   - Document skew angle estimation and bilinear rotation (`estimate_skew_angle_deg`, `deskew_image`).
//! - Real-time 256-bin RGB and Luminance histograms (`RgbHistogram`).
//! - Monotone cubic spline and gamma tone curve mapping (`ToneCurve`).
//! - Multi-page PDF generation via `pdf-writer`, PNG, JPEG, and TIFF document exporters (`export_pdf_multipage`, `export_png`, `export_jpeg`, `export_tiff`).
//! - End-to-end scanner pipeline execution (`execute_scan_pipeline`).

pub mod cis_align;
pub mod errors;
pub mod export;
pub mod geometry;
pub mod pipeline;
pub mod restoration;
pub mod tone;

pub use cis_align::LineStaggerBuffer;
pub use errors::ImageProcError;
pub use export::{
    export_jpeg, export_pdf_multipage, export_png, export_tiff, save_jpeg, save_pdf, save_png,
    save_tiff,
};
pub use geometry::{auto_multi_crop, deskew_image, estimate_skew_angle_deg, rotate_bilinear};
pub use pipeline::execute_scan_pipeline;
pub use restoration::{
    correct_gutter_shadow, descreen_moire_2d_fft, remove_dust_and_scratches, unsharp_mask,
};
pub use tone::{RgbHistogram, ToneCurve};
