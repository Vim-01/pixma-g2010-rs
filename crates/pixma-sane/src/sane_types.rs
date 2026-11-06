//! Standard SANE v1.04 C ABI type definitions and constants.
//!
//! Conforms strictly to the SANE (Scanner Access Now Easy) Standard Version 1.04.

#![allow(non_camel_case_types)]
#![allow(non_snake_case)]
#![allow(non_upper_case_globals)]

use std::ffi::c_char;

// ---------------------------------------------------------------------------
// Primitive Type Aliases matching SANE C ABI
// ---------------------------------------------------------------------------

pub type SANE_Byte = u8;
pub type SANE_Word = i32;
pub type SANE_Bool = SANE_Word;
pub type SANE_Int = SANE_Word;
pub type SANE_Fixed = SANE_Word;
pub type SANE_String = *mut c_char;
pub type SANE_String_Const = *const c_char;
pub type SANE_Char = c_char;
pub type SANE_Handle = *mut std::ffi::c_void;

pub type SANE_Auth_Callback = Option<
    unsafe extern "C" fn(
        resource: SANE_String_Const,
        username: *mut SANE_Char,
        password: *mut SANE_Char,
    ),
>;

// ---------------------------------------------------------------------------
// Boolean Constants
// ---------------------------------------------------------------------------

pub const SANE_FALSE: SANE_Bool = 0;
pub const SANE_TRUE: SANE_Bool = 1;

// ---------------------------------------------------------------------------
// SANE_Status
// ---------------------------------------------------------------------------

/// Return status codes from SANE operations.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Status {
    /// Operation completed successfully.
    Good = 0,
    /// Operation is not supported by backend.
    Unsupported = 1,
    /// Operation was cancelled by user.
    Cancelled = 2,
    /// Device is busy; retry later.
    DeviceBusy = 3,
    /// Data or argument is invalid.
    Inval = 4,
    /// End of scanned document/file reached.
    Eof = 5,
    /// Document feeder is jammed.
    Jammed = 6,
    /// Document feeder is out of documents.
    NoDocs = 7,
    /// Scanner cover is open.
    CoverOpen = 8,
    /// Error during device communication / I/O.
    IoError = 9,
    /// Insufficient memory to complete operation.
    NoMem = 10,
    /// Access to device was denied.
    AccessDenied = 11,
}

pub const SANE_STATUS_GOOD: SANE_Status = SANE_Status::Good;
pub const SANE_STATUS_UNSUPPORTED: SANE_Status = SANE_Status::Unsupported;
pub const SANE_STATUS_CANCELLED: SANE_Status = SANE_Status::Cancelled;
pub const SANE_STATUS_DEVICE_BUSY: SANE_Status = SANE_Status::DeviceBusy;
pub const SANE_STATUS_INVAL: SANE_Status = SANE_Status::Inval;
pub const SANE_STATUS_EOF: SANE_Status = SANE_Status::Eof;
pub const SANE_STATUS_JAMMED: SANE_Status = SANE_Status::Jammed;
pub const SANE_STATUS_NO_DOCS: SANE_Status = SANE_Status::NoDocs;
pub const SANE_STATUS_COVER_OPEN: SANE_Status = SANE_Status::CoverOpen;
pub const SANE_STATUS_IO_ERROR: SANE_Status = SANE_Status::IoError;
pub const SANE_STATUS_NO_MEM: SANE_Status = SANE_Status::NoMem;
pub const SANE_STATUS_ACCESS_DENIED: SANE_Status = SANE_Status::AccessDenied;

// ---------------------------------------------------------------------------
// SANE_Value_Type
// ---------------------------------------------------------------------------

/// Option value data types.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Value_Type {
    /// Boolean flag (0 or 1 stored in SANE_Word).
    Bool = 0,
    /// 32-bit signed integer.
    Int = 1,
    /// 16.16 signed fixed point representation.
    Fixed = 2,
    /// Null-terminated C string.
    String = 3,
    /// Action trigger without associated value.
    Button = 4,
    /// Option grouping header.
    Group = 5,
}

pub const SANE_TYPE_BOOL: SANE_Value_Type = SANE_Value_Type::Bool;
pub const SANE_TYPE_INT: SANE_Value_Type = SANE_Value_Type::Int;
pub const SANE_TYPE_FIXED: SANE_Value_Type = SANE_Value_Type::Fixed;
pub const SANE_TYPE_STRING: SANE_Value_Type = SANE_Value_Type::String;
pub const SANE_TYPE_BUTTON: SANE_Value_Type = SANE_Value_Type::Button;
pub const SANE_TYPE_GROUP: SANE_Value_Type = SANE_Value_Type::Group;

// ---------------------------------------------------------------------------
// SANE_Unit
// ---------------------------------------------------------------------------

/// Units of measurement for option values.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Unit {
    /// Unitless value.
    None = 0,
    /// Picture elements.
    Pixel = 1,
    /// Bits.
    Bit = 2,
    /// Millimeters.
    Mm = 3,
    /// Dots per inch.
    Dpi = 4,
    /// Percentage (0..100).
    Percent = 5,
    /// Microseconds.
    Microsecond = 6,
}

pub const SANE_UNIT_NONE: SANE_Unit = SANE_Unit::None;
pub const SANE_UNIT_PIXEL: SANE_Unit = SANE_Unit::Pixel;
pub const SANE_UNIT_BIT: SANE_Unit = SANE_Unit::Bit;
pub const SANE_UNIT_MM: SANE_Unit = SANE_Unit::Mm;
pub const SANE_UNIT_DPI: SANE_Unit = SANE_Unit::Dpi;
pub const SANE_UNIT_PERCENT: SANE_Unit = SANE_Unit::Percent;
pub const SANE_UNIT_MICROSECOND: SANE_Unit = SANE_Unit::Microsecond;

// ---------------------------------------------------------------------------
// SANE_Constraint_Type
// ---------------------------------------------------------------------------

/// Types of constraints applicable to option values.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Constraint_Type {
    /// No constraint; any value valid for type is allowed.
    None = 0,
    /// Bounded by a range (min, max, quant).
    Range = 1,
    /// Bounded to a discrete list of words.
    WordList = 2,
    /// Bounded to a list of allowed strings.
    StringList = 3,
}

pub const SANE_CONSTRAINT_NONE: SANE_Constraint_Type = SANE_Constraint_Type::None;
pub const SANE_CONSTRAINT_RANGE: SANE_Constraint_Type = SANE_Constraint_Type::Range;
pub const SANE_CONSTRAINT_WORD_LIST: SANE_Constraint_Type = SANE_Constraint_Type::WordList;
pub const SANE_CONSTRAINT_STRING_LIST: SANE_Constraint_Type = SANE_Constraint_Type::StringList;

// ---------------------------------------------------------------------------
// SANE_Action
// ---------------------------------------------------------------------------

/// Actions for `sane_control_option`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Action {
    /// Read the current option value.
    GetValue = 0,
    /// Assign a new option value.
    SetValue = 1,
    /// Revert option value to automatic setting.
    SetAuto = 2,
}

pub const SANE_ACTION_GET_VALUE: SANE_Action = SANE_Action::GetValue;
pub const SANE_ACTION_SET_VALUE: SANE_Action = SANE_Action::SetValue;
pub const SANE_ACTION_SET_AUTO: SANE_Action = SANE_Action::SetAuto;

// ---------------------------------------------------------------------------
// SANE_Frame
// ---------------------------------------------------------------------------

/// Image transmission frame formats.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SANE_Frame {
    /// Single channel grayscale image or lineart.
    Gray = 0,
    /// Interleaved 24-bit RGB pixel stream.
    Rgb = 1,
    /// Red color channel.
    Red = 2,
    /// Green color channel.
    Green = 3,
    /// Blue color channel.
    Blue = 4,
}

pub const SANE_FRAME_GRAY: SANE_Frame = SANE_Frame::Gray;
pub const SANE_FRAME_RGB: SANE_Frame = SANE_Frame::Rgb;
pub const SANE_FRAME_RED: SANE_Frame = SANE_Frame::Red;
pub const SANE_FRAME_GREEN: SANE_Frame = SANE_Frame::Green;
pub const SANE_FRAME_BLUE: SANE_Frame = SANE_Frame::Blue;

// ---------------------------------------------------------------------------
// Option Capability Flags
// ---------------------------------------------------------------------------

pub const SANE_CAP_SOFT_SELECT: SANE_Int = 1 << 0;
pub const SANE_CAP_HARD_SELECT: SANE_Int = 1 << 1;
pub const SANE_CAP_SOFT_DETECT: SANE_Int = 1 << 2;
pub const SANE_CAP_EMULATED: SANE_Int = 1 << 3;
pub const SANE_CAP_AUTOMATIC: SANE_Int = 1 << 4;
pub const SANE_CAP_INACTIVE: SANE_Int = 1 << 5;
pub const SANE_CAP_ADVANCED: SANE_Int = 1 << 6;

// ---------------------------------------------------------------------------
// Control Option Info Flags
// ---------------------------------------------------------------------------

pub const SANE_INFO_INEXACT: SANE_Int = 1 << 0;
pub const SANE_INFO_RELOAD_OPTIONS: SANE_Int = 1 << 1;
pub const SANE_INFO_RELOAD_PARAMS: SANE_Int = 1 << 2;

// ---------------------------------------------------------------------------
// SANE Range Structure
// ---------------------------------------------------------------------------

/// Bounding range with quantization step.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SANE_Range {
    pub min: SANE_Word,
    pub max: SANE_Word,
    pub quant: SANE_Word,
}

// ---------------------------------------------------------------------------
// SANE Device Descriptor
// ---------------------------------------------------------------------------

/// Hardware scanner descriptor returned by `sane_get_devices`.
#[repr(C)]
#[derive(Debug, Clone)]
pub struct SANE_Device {
    pub name: SANE_String_Const,
    pub vendor: SANE_String_Const,
    pub model: SANE_String_Const,
    pub type_: SANE_String_Const,
}

unsafe impl Sync for SANE_Device {}
unsafe impl Send for SANE_Device {}

// ---------------------------------------------------------------------------
// SANE Option Descriptor Union & Struct
// ---------------------------------------------------------------------------

/// C ABI union representing the constraint container.
#[repr(C)]
#[derive(Copy, Clone)]
pub union SANE_Constraint {
    pub string_list: *const SANE_String_Const,
    pub word_list: *const SANE_Word,
    pub range: *const SANE_Range,
}

unsafe impl Sync for SANE_Constraint {}
unsafe impl Send for SANE_Constraint {}

/// Metadata descriptor for a single scanner option.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct SANE_Option_Descriptor {
    pub name: SANE_String_Const,
    pub title: SANE_String_Const,
    pub desc: SANE_String_Const,
    pub type_: SANE_Value_Type,
    pub unit: SANE_Unit,
    pub size: SANE_Int,
    pub cap: SANE_Int,
    pub constraint_type: SANE_Constraint_Type,
    pub constraint: SANE_Constraint,
}

unsafe impl Sync for SANE_Option_Descriptor {}
unsafe impl Send for SANE_Option_Descriptor {}

// ---------------------------------------------------------------------------
// SANE Parameters
// ---------------------------------------------------------------------------

/// Image transmission parameters queried by `sane_get_parameters`.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SANE_Parameters {
    pub format: SANE_Frame,
    pub last_frame: SANE_Bool,
    pub bytes_per_line: SANE_Int,
    pub pixels_per_line: SANE_Int,
    pub lines: SANE_Int,
    pub depth: SANE_Int,
}

// ---------------------------------------------------------------------------
// Fixed-Point and Version Code Helpers
// ---------------------------------------------------------------------------

/// Converts a floating-point value to 16.16 signed fixed point (`SANE_Fixed`).
#[inline]
pub fn sane_fix(v: f64) -> SANE_Fixed {
    ((v * 65536.0) + 0.5) as SANE_Fixed
}

/// Converts a 16.16 signed fixed point (`SANE_Fixed`) to a floating-point value.
#[inline]
pub fn sane_unfix(v: SANE_Fixed) -> f64 {
    (v as f64) / 65536.0
}

/// Computes the standard SANE 32-bit version code: `(major << 24) | (minor << 16) | build`.
#[inline]
pub const fn sane_version_code(major: u8, minor: u8, build: u16) -> SANE_Int {
    (((major as SANE_Int) & 0xFF) << 24)
        | (((minor as SANE_Int) & 0xFF) << 16)
        | ((build as SANE_Int) & 0xFFFF)
}

/// Wrapper to safely make static pointer arrays `Sync`.
#[repr(transparent)]
pub struct SyncWrapper<T>(pub T);
unsafe impl<T> Sync for SyncWrapper<T> {}
unsafe impl<T> Send for SyncWrapper<T> {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sane_version_code() {
        let code = sane_version_code(1, 0, 4);
        assert_eq!(code, (1 << 24) | 4);
    }

    #[test]
    fn test_sane_fixed_conversion() {
        let original = 215.9;
        let fixed = sane_fix(original);
        let back = sane_unfix(fixed);
        assert!((original - back).abs() < 0.001);

        assert_eq!(sane_fix(0.0), 0);
        assert_eq!(sane_unfix(0), 0.0);
    }

    #[test]
    fn test_option_descriptor_layout() {
        assert_eq!(
            std::mem::size_of::<SANE_Option_Descriptor>(),
            std::mem::size_of::<usize>() * 3
                + 4 * 5
                + std::mem::size_of::<usize>() // union
                + 4 // padding to align union
        );
    }
}
