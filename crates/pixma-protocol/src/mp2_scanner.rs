//! Canon MP2 binary scanner protocol encoder and chunk response parser.
//!
//! Binary scanner commands operate over USB Interface 0 Bulk endpoints after
//! completing the IVEC XML handshake and ModeShift.

use crate::errors::ProtocolError;
use pixma_core::scan_params::ScanParameters;

/// MP2 scanner binary command opcodes (big-endian 16-bit integers).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum Mp2Opcode {
    GetDataStatus = 0xF320,
    GetCalibrationStatus = 0xDA20,
    GetOpuMode = 0xDC20,
    StartSession = 0xDB20,
    SendGammaTable = 0xEE20,
    SetLamp = 0xD520,
    SetScanParameter = 0xD820,
    StartScan = 0xD920,
    ReadScanChunk = 0xD420,
    CancelOrFinishScan = 0xEF20,
}

impl Mp2Opcode {
    pub const fn as_u16(self) -> u16 {
        self as u16
    }
}

/// Decoded image chunk packet received in response to `ReadScanChunk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanChunkPacket {
    pub is_page_end: bool,
    pub is_scan_complete: bool,
    pub status_raw: u8,
    pub payload: Vec<u8>,
}

/// Computes the two's-complement MP2 payload checksum over `payload_without_checksum`.
pub fn compute_mp2_checksum(payload_without_checksum: &[u8]) -> u8 {
    let sum = payload_without_checksum
        .iter()
        .copied()
        .fold(0u8, u8::wrapping_add);
    0u8.wrapping_sub(sum)
}

/// Builds a standard 16-byte MP2 command header.
pub fn build_16byte_cmd(opcode: Mp2Opcode, param3: u8) -> [u8; 16] {
    let mut buf = [0u8; 16];
    let op_bytes = (opcode as u16).to_be_bytes();
    buf[0] = op_bytes[0];
    buf[1] = op_bytes[1];
    buf[3] = param3;
    buf
}

pub fn build_start_session() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::StartSession, 0x00)
}

/// Builds the 2072-byte (`0x818`) `0xEE20` gamma table packet for Generation 5 PIXMA scanners.
pub fn build_gamma_table(gamma: f64) -> Vec<u8> {
    let mut buf = vec![0u8; 2072];
    let op = (Mp2Opcode::SendGammaTable as u16).to_be_bytes();
    buf[0] = op[0];
    buf[1] = op[1];
    // 2056 (0x0808) payload bytes after the 16-byte command header
    buf[14] = 0x08;
    buf[15] = 0x08;
    // Gamma header: [0x10, 0x00, 0x08, 0x04] followed by 1024 LE u16 entries at buf[20..2068]
    buf[16] = 0x10;
    buf[17] = 0x00;
    buf[18] = 0x08;
    buf[19] = 0x04;
    let inv_gamma = 1.0 / gamma.max(0.1);
    for i in 0..1024usize {
        let normalized = (i as f64) / 1023.0;
        let val = (normalized.powf(inv_gamma) * 65535.0)
            .round()
            .clamp(0.0, 65535.0) as u16;
        let le = val.to_le_bytes();
        let off = 20 + i * 2;
        buf[off] = le[0];
        buf[off + 1] = le[1];
    }
    buf[2071] = compute_mp2_checksum(&buf[16..2071]);
    buf
}

pub fn build_get_data_status() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::GetDataStatus, 0x00)
}

pub fn build_get_calibration_status() -> [u8; 16] {
    let mut buf = build_16byte_cmd(Mp2Opcode::GetCalibrationStatus, 0x00);
    buf[15] = 0x08;
    buf
}

pub fn build_get_opu_mode() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::GetOpuMode, 0x00)
}

pub fn build_set_lamp(on: bool) -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::SetLamp, if on { 0x01 } else { 0x00 })
}

pub fn build_start_scan() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::StartScan, 0x00)
}

pub fn build_read_scan_chunk() -> [u8; 16] {
    build_read_scan_chunk_with_req(0x0008_0008)
}

pub fn build_read_scan_chunk_with_req(req_word: u32) -> [u8; 16] {
    let mut buf = build_16byte_cmd(Mp2Opcode::ReadScanChunk, 0x00);
    buf[12..16].copy_from_slice(&req_word.to_be_bytes());
    buf
}

pub fn build_cancel_or_finish_scan() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::CancelOrFinishScan, 0x00)
}

/// Computes the 32-pixel aligned hardware CIS scanline width (`wx`) required by
/// Generation 5 MP2 scanners (`(width + (x_offset % 32) + 31) & !31`).
pub fn aligned_scan_width(params: &ScanParameters) -> u32 {
    let xs = params.geometry.x_offset & 31;
    ((params.geometry.width + xs + 31) & !31).max(32)
}

/// Generates the 72-byte (`0x48`) `SetScanParameter` command packet.
///
/// Validates scan geometry bounds against physical flatbed limits for the requested
/// DPI before encoding big-endian parameter fields and the MP2 payload checksum.
pub fn build_set_scan_parameter(params: &ScanParameters) -> Result<[u8; 72], ProtocolError> {
    params.geometry.validate(params.resolution)?;
    let aligned_x = params.geometry.x_offset & !31;
    let raw_width = aligned_scan_width(params);

    let mut buf = [0u8; 72];

    // Opcode at 0x00..0x02 and payload length 0x0038 (56 bytes) at 0x0E..0x10
    let op = (Mp2Opcode::SetScanParameter as u16).to_be_bytes();
    buf[0] = op[0];
    buf[1] = op[1];
    buf[14] = 0x00;
    buf[15] = 0x38;

    // Mode bytes at 0x10..0x16 (Generation 5 flatbed scan framing)
    buf[0x10..0x14].copy_from_slice(&[0x01, 0x01, 0x01, 0x00]);
    buf[0x15] = 0x01;

    // Resolutions (DPI | 0x8000) at 0x18..0x1C
    let dpi_code = (params.resolution.as_dpi() | 0x8000).to_be_bytes();
    buf[0x18..0x1A].copy_from_slice(&dpi_code);
    buf[0x1A..0x1C].copy_from_slice(&dpi_code);

    // Offsets and dimensions at 0x1C..0x2C
    buf[0x1C..0x20].copy_from_slice(&aligned_x.to_be_bytes());
    buf[0x20..0x24].copy_from_slice(&params.geometry.y_offset.to_be_bytes());
    buf[0x24..0x28].copy_from_slice(&raw_width.to_be_bytes());
    buf[0x28..0x2C].copy_from_slice(&params.geometry.height.to_be_bytes());

    // Color depth and bits per pixel at 0x2C..0x2E
    buf[0x2C] = params.color_mode.depth_byte();
    buf[0x2D] = params.color_mode.bits_per_pixel_byte();
    buf[0x2F] = 0x01;
    buf[0x30] = 0xFF;

    // Gamma code at 0x31..0x35 (0x81, 0x00, 0x02, 0x01 -> gamma 2.2)
    buf[0x31] = 0x81;
    buf[0x32] = 0x00;
    buf[0x33] = 0x02;
    buf[0x34] = 0x01;
    buf[0x40] = 0x01;

    // Payload checksum at 0x47 (byte 71)
    buf[0x47] = compute_mp2_checksum(&buf[16..71]);

    Ok(buf)
}

/// Parses a `ReadScanChunk` response buffer from the scanner bulk IN pipe.
///
/// Verifies `[0x06, 0x06]` ACK status prefix, extracts status flags from both legacy
/// (`buf[2..4]`) and Generation 5 (`buf[8]`) offsets, and returns the image payload slice.
pub fn parse_scan_chunk_response(buf: &[u8]) -> Result<ScanChunkPacket, ProtocolError> {
    if buf.len() < 16 {
        return Err(ProtocolError::BufferTooShort {
            expected: 16,
            actual: buf.len(),
        });
    }

    if buf[0] != 0x06 || buf[1] != 0x06 {
        return Err(ProtocolError::InvalidAck {
            actual_0: buf[0],
            actual_1: buf[1],
        });
    }

    let status_raw = if buf[8] != 0 { buf[8] } else { buf[2] };
    let is_page_end = (status_raw & 0x20 != 0) || (buf[3] & 0x20 != 0) || (buf[8] & 0x20 != 0);
    let is_scan_complete = status_raw == 0x28
        || status_raw == 0x38
        || buf[3] == 0x28
        || buf[3] == 0x38
        || buf[8] == 0x28
        || buf[8] == 0x38;

    let payload_len = u32::from_be_bytes(buf[12..16].try_into().unwrap()) as usize;
    let total_expected = 16 + payload_len;

    if buf.len() < total_expected {
        return Err(ProtocolError::BufferTooShort {
            expected: total_expected,
            actual: buf.len(),
        });
    }

    let payload = buf[16..total_expected].to_vec();

    Ok(ScanChunkPacket {
        is_page_end,
        is_scan_complete,
        status_raw,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanResolution};

    #[test]
    fn builds_16byte_commands_correctly() {
        let lamp_on = build_set_lamp(true);
        assert_eq!(lamp_on[0..2], [0xD5, 0x20]);
        assert_eq!(lamp_on[3], 0x01);

        let lamp_off = build_set_lamp(false);
        assert_eq!(lamp_off[3], 0x00);

        let start = build_start_scan();
        assert_eq!(start[0..2], [0xD9, 0x20]);
        assert_eq!(start[3], 0x00);

        let read = build_read_scan_chunk();
        assert_eq!(read[0..2], [0xD4, 0x20]);
        assert_eq!(&read[12..16], &[0x00, 0x08, 0x00, 0x08]);
    }

    #[test]
    fn builds_set_scan_parameter_72_bytes() {
        let params = ScanParameters {
            resolution: ScanResolution::Dpi600,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry {
                x_offset: 100,
                y_offset: 200,
                width: 1000,
                height: 1500,
            },
            ..Default::default()
        };

        let cmd = build_set_scan_parameter(&params).unwrap();
        assert_eq!(cmd.len(), 72);
        assert_eq!(&cmd[0..2], &[0xD8, 0x20]);
        assert_eq!(&cmd[14..16], &[0x00, 0x38]);
        assert_eq!(&cmd[0x10..0x16], &[0x01, 0x01, 0x01, 0x00, 0x00, 0x01]);

        let expected_dpi = (600u16 | 0x8000).to_be_bytes();
        assert_eq!(&cmd[0x18..0x1A], &expected_dpi);
        assert_eq!(&cmd[0x1A..0x1C], &expected_dpi);

        assert_eq!(&cmd[0x1C..0x20], &96u32.to_be_bytes());
        assert_eq!(&cmd[0x20..0x24], &200u32.to_be_bytes());
        assert_eq!(&cmd[0x24..0x28], &1024u32.to_be_bytes());
        assert_eq!(&cmd[0x28..0x2C], &1500u32.to_be_bytes());

        assert_eq!(cmd[0x2C], 0x08);
        assert_eq!(cmd[0x2D], 0x18);
        assert_eq!(&cmd[0x31..0x33], &[0x81, 0x00]);
    }

    #[test]
    fn rejects_out_of_bounds_geometry() {
        let params = ScanParameters {
            resolution: ScanResolution::Dpi300,
            geometry: ScanGeometry {
                x_offset: 5000,
                y_offset: 0,
                width: 1000,
                height: 1000,
            },
            ..Default::default()
        };

        assert!(build_set_scan_parameter(&params).is_err());
    }

    #[test]
    fn parses_valid_scan_chunk_response() {
        let mut response = vec![0u8; 16 + 8];
        response[0] = 0x06;
        response[1] = 0x06;
        response[2] = 0x28; // Scan complete
        response[12..16].copy_from_slice(&8u32.to_be_bytes()); // length 8
        response[16..24].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);

        let packet = parse_scan_chunk_response(&response).unwrap();
        assert!(packet.is_scan_complete);
        assert_eq!(packet.payload, vec![1, 2, 3, 4, 5, 6, 7, 8]);
    }

    #[test]
    fn rejects_invalid_ack() {
        let mut response = vec![0u8; 16];
        response[0] = 0x15; // NAK
        assert!(matches!(
            parse_scan_chunk_response(&response),
            Err(ProtocolError::InvalidAck { .. })
        ));
    }
}
