//! Error types for the scanner image post-processing engine.

use pixma_core::errors::HardwareConstraintViolation;
use thiserror::Error;

/// Errors produced during scanner image restoration, geometry transformations, and export.
#[derive(Debug, Error)]
pub enum ImageProcError {
    /// Attempted to process or export an image with zero width or height.
    #[error("Empty image: width and height must be strictly positive")]
    EmptyImage,

    /// Raw scanline buffer length did not match expected byte count.
    #[error("Invalid line byte length: expected {expected}, actual {actual}")]
    InvalidLineLength { expected: usize, actual: usize },

    /// Document export encoding failed (PNG, JPEG, TIFF).
    #[error("Image encoding error: {0}")]
    EncodingError(String),

    /// Multi-page PDF generation error.
    #[error("PDF generation error: {0}")]
    PdfError(String),

    /// Invalid tone curve control points or spline knots.
    #[error("Invalid control points: {0}")]
    InvalidControlPoints(String),

    /// Hardware constraint violation propagated from `pixma-core`.
    #[error("Hardware constraint violation: {0}")]
    HardwareConstraintViolation(#[from] HardwareConstraintViolation),

    /// Error propagated from underlying `image` crate codec.
    #[error("Image crate error: {0}")]
    ImageError(#[from] image::ImageError),

    /// Standard I/O error during file read or write.
    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),
}
