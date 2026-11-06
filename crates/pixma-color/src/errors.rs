//! Error definitions for color transformations and ink separation.

use thiserror::Error;

/// Errors arising during color pipeline and ICC transform operations.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ColorError {
    /// Failed to decode or parse an ICC color profile.
    #[error("Failed to parse ICC profile")]
    InvalidIccProfile,

    /// Failed to initialize ICC transform between profiles.
    #[error("Failed to create color transform")]
    TransformCreationFailed,

    /// Pixel or channel buffer length mismatch.
    #[error("Buffer length mismatch: expected {expected}, got {actual}")]
    BufferLengthMismatch {
        /// Expected byte length.
        expected: usize,
        /// Actual byte length provided.
        actual: usize,
    },
}
