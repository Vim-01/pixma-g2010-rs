//! Canon PIXMA G2010 wire protocols, framing, and command encoders.
//!
//! Implements clean-room protocol serialization and deserialization for:
//! - **BJL (Bubble Jet Level):** Time synchronization, command envelopes, and ESC/P prefix switching.
//! - **CNCL Codec:** PPD capability cipher using XOR `0x39`, `'a'..'p'` nibble encoding, and 32-bit checksums.
//! - **IVEC XML:** Job lifecycle (`StartJob`, `SetJobConfiguration`, `SetPageConfiguration`), maintenance routines, and telemetry status queries.
//! - **PWG Raster (`RaS2`):** Header parsing, PackBits run-length compression, page streaming, and IPC framing.
//! - **BJRaster3:** Direct Canon ESC/P raster stream encoding with safety enforcement.
//! - **MP2 Scanner:** Binary scanner commands, 72-byte geometry configuration, and scan chunk decoding.

pub mod bjl;
pub mod bjraster3;
pub mod cncl_codec;
pub mod errors;
pub mod ivec;
pub mod mp2_scanner;
pub mod pwg_raster;

pub use bjl::{BjlBuilder, BJL_ENTRY_PREFIX};
pub use bjraster3::{BjRaster3Encoder, FORM_FEED};
pub use cncl_codec::{decode_cncl_string, encode_cncl_string};
pub use errors::ProtocolError;
pub use ivec::{
    build_bjl_set_time_send_data, build_cancel_job, build_cleaning_cmd, build_end_job,
    build_get_status_cmd, build_manual_alignment_values_cmd, build_mode_shift,
    build_roller_cleaning_cmd, build_send_data_header, build_set_job_configuration,
    build_set_page_configuration, build_start_job, build_test_print_cmd, parse_status_response,
    IVEC_COMMON_NS, VCN_CANON_NS,
};
pub use mp2_scanner::{
    build_cancel_or_finish_scan, build_get_calibration_status, build_get_data_status,
    build_get_opu_mode, build_read_scan_chunk, build_set_lamp, build_set_scan_parameter,
    build_start_scan, parse_scan_chunk_response, Mp2Opcode, ScanChunkPacket,
};
pub use pwg_raster::{
    decode_packbits_scanline, encode_packbits_scanline, encode_pwg_page, CnclIpcFrameHeader,
    PwgRasterHeader, CNCL_IPC_FRAME_SIZE, CNCL_IPC_MAGIC, PWG_HEADER_SIZE, PWG_RASTER_MAGIC,
};
