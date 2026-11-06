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

/// Builds a standard 16-byte MP2 command header.
pub fn build_16byte_cmd(opcode: Mp2Opcode, param3: u8) -> [u8; 16] {
    let mut buf = [0u8; 16];
    let op_bytes = (opcode as u16).to_be_bytes();
    buf[0] = op_bytes[0];
    buf[1] = op_bytes[1];
    buf[3] = param3;
    buf
}

pub fn build_get_data_status() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::GetDataStatus, 0x00)
}

pub fn build_get_calibration_status() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::GetCalibrationStatus, 0x00)
}

pub fn build_get_opu_mode() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::GetOpuMode, 0x00)
}

pub fn build_set_lamp(on: bool) -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::SetLamp, if on { 0x01 } else { 0x00 })
}

pub fn build_start_scan() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::StartScan, 0x01)
}

pub fn build_read_scan_chunk() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::ReadScanChunk, 0x00)
}

pub fn build_cancel_or_finish_scan() -> [u8; 16] {
    build_16byte_cmd(Mp2Opcode::CancelOrFinishScan, 0x00)
}

/// Generates the 72-byte (`0x48`) `SetScanParameter` command packet.
///
/// Validates scan geometry bounds against physical flatbed limits for the requested
/// DPI before encoding big-endian parameter fields.
pub fn build_set_scan_parameter(params: &ScanParameters) -> Result<[u8; 72], ProtocolError> {
    params.geometry.validate(params.resolution)?;

    let mut buf = [0u8; 72];

    // Opcode at 0x00..0x02
    let op = (Mp2Opcode::SetScanParameter as u16).to_be_bytes();
    buf[0] = op[0];
    buf[1] = op[1];

    // Mode bytes at 0x10..0x14
    buf[0x10..0x14].copy_from_slice(&params.color_mode.mp2_mode_bytes());

    // Resolutions (DPI | 0x8000) at 0x18..0x1C
    let dpi_code = (params.resolution.as_dpi() | 0x8000).to_be_bytes();
    buf[0x18..0x1A].copy_from_slice(&dpi_code);
    buf[0x1A..0x1C].copy_from_slice(&dpi_code);

    // Offsets and dimensions at 0x1C..0x2C
    buf[0x1C..0x20].copy_from_slice(&params.geometry.x_offset.to_be_bytes());
    buf[0x20..0x24].copy_from_slice(&params.geometry.y_offset.to_be_bytes());
    buf[0x24..0x28].copy_from_slice(&params.geometry.width.to_be_bytes());
    buf[0x28..0x2C].copy_from_slice(&params.geometry.height.to_be_bytes());

    // Color depth and bits per pixel at 0x2C..0x2E
    buf[0x2C] = params.color_mode.depth_byte();
    buf[0x2D] = params.color_mode.bits_per_pixel_byte();

    // Gamma code at 0x31..0x33 (0x81, 0x00 -> gamma 2.2)
    buf[0x31] = 0x81;
    buf[0x32] = 0x00;

    Ok(buf)
}

/// Parses a `ReadScanChunk` response buffer from the scanner bulk IN pipe.
///
/// Verifies `[0x06, 0x06]` ACK status prefix, extracts status flags, and returns
/// the image payload slice.
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

    let status_raw = buf[2];
    let is_page_end = (status_raw & 0x20 != 0) || (buf[3] & 0x20 != 0);
    let is_scan_complete =
        status_raw == 0x28 || status_raw == 0x38 || buf[3] == 0x28 || buf[3] == 0x38;

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
        assert_eq!(start[3], 0x01);

        let read = build_read_scan_chunk();
        assert_eq!(read[0..2], [0xD4, 0x20]);
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
        assert_eq!(&cmd[0x10..0x14], &[0x02, 0x01, 0x03, 0x01]);

        let expected_dpi = (600u16 | 0x8000).to_be_bytes();
        assert_eq!(&cmd[0x18..0x1A], &expected_dpi);
        assert_eq!(&cmd[0x1A..0x1C], &expected_dpi);

        assert_eq!(&cmd[0x1C..0x20], &100u32.to_be_bytes());
        assert_eq!(&cmd[0x20..0x24], &200u32.to_be_bytes());
        assert_eq!(&cmd[0x24..0x28], &1000u32.to_be_bytes());
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
