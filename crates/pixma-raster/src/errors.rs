//! Error types for the rasterization, layout, halftoning, and print pipeline.

use thiserror::Error;

/// Errors that can occur during raster reading, layout processing, dithering, or job compilation.
#[derive(Debug, Error)]
pub enum RasterError {
    /// No pages were provided to the pipeline or layout engine.
    #[error("No pages provided")]
    NoPages,

    /// Page dimensions are invalid (zero width or height).
    #[error("Invalid page dimensions: width={width}, height={height}")]
    InvalidDimensions { width: u32, height: u32 },

    /// The pixel buffer length does not match the expected `width * height * bpp`.
    #[error("Buffer length mismatch: expected {expected} bytes, got {actual} bytes")]
    BufferLengthMismatch { expected: usize, actual: usize },

    /// Layout configuration parameter is out of bounds or unsupported.
    #[error("Invalid layout configuration: {0}")]
    InvalidLayout(String),

    /// Image decoding failed.
    #[error("Image decode error: {0}")]
    ImageDecode(#[from] image::ImageError),

    /// Wire protocol serialization or framing error.
    #[error("Protocol error: {0}")]
    Protocol(#[from] pixma_protocol::ProtocolError),

    /// Physical carriage bounds, page height, or resolution safety violation.
    #[error("Hardware constraint violation: {0}")]
    HardwareConstraint(#[from] pixma_core::HardwareConstraintViolation),

    /// Color profile or LUT transform error.
    #[error("Color processing error: {0}")]
    Color(#[from] pixma_color::ColorError),

    /// Malformed raster stream or unexpected end-of-file during parsing.
    #[error("Stream parsing error: {0}")]
    StreamParse(String),

    /// Dithering or halftoning processing error.
    #[error("Halftone error: {0}")]
    Halftone(String),
}
