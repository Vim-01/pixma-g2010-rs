//! Protocol serialization and parsing error types.

use pixma_core::errors::HardwareConstraintViolation;
use thiserror::Error;

/// Protocol serialization, parsing, and wire-framing errors.
#[derive(Debug, Error, PartialEq)]
pub enum ProtocolError {
    #[error(transparent)]
    HardwareConstraint(#[from] HardwareConstraintViolation),

    #[error("CNCL string contains invalid character '{0}' (expected 'a'..'p' or '=')")]
    CnclInvalidCharacter(char),

    #[error("CNCL format error: {0}")]
    CnclInvalidFormat(String),

    #[error("CNCL 32-bit checksum mismatch: expected {expected:#010X}, got {actual:#010X}")]
    CnclChecksumMismatch { expected: u32, actual: u32 },

    #[error("Magic mismatch: expected {expected:02X?}, got {actual:02X?}")]
    InvalidMagic { expected: [u8; 4], actual: [u8; 4] },

    #[error("Buffer too short: expected at least {expected} bytes, got {actual}")]
    BufferTooShort { expected: usize, actual: usize },

    #[error("XML protocol error: {0}")]
    Xml(String),

    #[error("Missing required XML tag or attribute: {0}")]
    MissingField(&'static str),

    #[error("Invalid value encountered: {0}")]
    InvalidValue(String),

    #[error(
        "MP2 scanner ACK mismatch: expected [0x06, 0x06], got [{actual_0:#04X}, {actual_1:#04X}]"
    )]
    InvalidAck { actual_0: u8, actual_1: u8 },

    #[error("Invalid MP2 opcode: {0:#06X}")]
    InvalidOpcode(u16),

    #[error("PackBits decoding overrun: buffer underflow or invalid control run")]
    PackBitsOverrun,
}
