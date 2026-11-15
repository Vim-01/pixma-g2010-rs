//! CUPS integration filters, command translator, USB backend, and option parser
//! for the Canon PIXMA G2010 (and compatible G-series MegaTank devices).

pub mod backend;
pub mod commands;
pub mod options;

pub use backend::{
    dispatch_cups_payload, format_cups_discovery_lines, format_cups_marker_attributes,
};
pub use commands::{
    compile_cups_command_stream, parse_cups_command_file, validate_and_compile_cups_commands,
    CupsCommand, CUPS_INK_FLUSH_CONFIRMED_END, CUPS_INK_FLUSH_CONFIRMED_START,
};
pub use options::parse_cups_options;
