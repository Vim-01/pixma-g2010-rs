//! PWG Raster (`RaS2`), PackBits scanline compression, and Canon IPC frame framing.
//!
//! Conforms to PWG 5102.4 Raster format specifications for Canon G2010 CUPS/IPP
//! raster streaming.

use crate::errors::ProtocolError;
use pixma_core::safety::ValidatedRasterLine;

/// PWG Raster 4-byte stream magic (`"RaS2"`).
pub const PWG_RASTER_MAGIC: [u8; 4] = *b"RaS2";

/// Size of PWG Raster per-page header in bytes (1796 bytes = 0x704).
pub const PWG_HEADER_SIZE: usize = 1796;

/// Size of Canon CNCL IPC frame header in bytes (136 bytes = 0x88).
pub const CNCL_IPC_FRAME_SIZE: usize = 136;

/// CNCL IPC magic constant: `0x12340001`.
pub const CNCL_IPC_MAGIC: u32 = 0x12340001;

/// Standard PWG Raster per-page header (1796 bytes).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PwgRasterHeader {
    pub media_class: String,
    pub media_type: String,
    pub dpi_x: u32,
    pub dpi_y: u32,
    pub imaging_bbox: (u32, u32, u32, u32),
    pub num_copies: u32,
    pub page_size_pts: (u32, u32),
    pub width_px: u32,
    pub height_px: u32,
    pub bits_per_color: u32,
    pub bits_per_pixel: u32,
    pub bytes_per_line: u32,
    pub color_space: u32,
    pub num_colors: u32,
    pub total_pages: u32,
    pub cups_integer: [u32; 16],
    pub rendering_intent: String,
    pub page_size_name: String,
}

impl PwgRasterHeader {
    /// Creates a 24-bit sRGB page header.
    pub fn new_srgb(width_px: u32, height_px: u32, dpi: u32) -> Self {
        let bytes_per_line = width_px * 3;
        let mut cups_integer = [0u32; 16];
        cups_integer[0] = 1; // TotalPageCount
        cups_integer[3] = 5782; // ImageBoxLeft
        cups_integer[4] = 8622; // ImageBoxTop
        cups_integer[5] = 351552; // ImageBoxRight
        cups_integer[6] = 500031; // ImageBoxBottom
        cups_integer[7] = 16777215; // AlternatePrimary

        Self {
            media_class: "PwgRaster".to_string(),
            media_type: "plain".to_string(),
            dpi_x: dpi,
            dpi_y: dpi,
            imaging_bbox: (10, 14, 586, 833),
            num_copies: 1,
            page_size_pts: (595, 842),
            width_px,
            height_px,
            bits_per_color: 8,
            bits_per_pixel: 24,
            bytes_per_line,
            color_space: 19, // sRGB
            num_colors: 3,
            total_pages: 1,
            cups_integer,
            rendering_intent: "auto".to_string(),
            page_size_name: "A4".to_string(),
        }
    }

    /// Creates an 8-bit SGray (monochrome) page header.
    pub fn new_sgray(width_px: u32, height_px: u32, dpi: u32) -> Self {
        let bytes_per_line = width_px;
        let mut cups_integer = [0u32; 16];
        cups_integer[0] = 1;
        cups_integer[3] = 5782;
        cups_integer[4] = 8622;
        cups_integer[5] = 351552;
        cups_integer[6] = 500031;
        cups_integer[7] = 16777215;

        Self {
            media_class: "PwgRaster".to_string(),
            media_type: "plain".to_string(),
            dpi_x: dpi,
            dpi_y: dpi,
            imaging_bbox: (10, 14, 586, 833),
            num_copies: 1,
            page_size_pts: (595, 842),
            width_px,
            height_px,
            bits_per_color: 8,
            bits_per_pixel: 8,
            bytes_per_line,
            color_space: 18, // SGray
            num_colors: 1,
            total_pages: 1,
            cups_integer,
            rendering_intent: "auto".to_string(),
            page_size_name: "A4".to_string(),
        }
    }

    /// Configures page geometry and media attributes matching Canon PPD definitions.
    pub fn with_media_config(
        mut self,
        media_type: &str,
        page_size_name: &str,
        page_size_pts: (u32, u32),
        imaging_bbox: (u32, u32, u32, u32),
        cups_integer: [u32; 16],
    ) -> Self {
        self.media_type = media_type.to_string();
        self.page_size_name = page_size_name.to_string();
        self.page_size_pts = page_size_pts;
        self.imaging_bbox = imaging_bbox;
        self.cups_integer = cups_integer;
        self
    }

    /// Serializes this header into the standard 1796-byte PWG Raster page header.
    pub fn to_bytes(&self) -> [u8; PWG_HEADER_SIZE] {
        let mut buf = [0u8; PWG_HEADER_SIZE];

        // MediaClass at 0..64
        let media_class = self.media_class.as_bytes();
        let class_len = media_class.len().min(63);
        buf[0..class_len].copy_from_slice(&media_class[..class_len]);

        // MediaType at 128..192
        let media_type = self.media_type.as_bytes();
        let type_len = media_type.len().min(63);
        buf[128..128 + type_len].copy_from_slice(&media_type[..type_len]);

        // Resolution at offsets 276..284
        buf[276..280].copy_from_slice(&self.dpi_x.to_be_bytes());
        buf[280..284].copy_from_slice(&self.dpi_y.to_be_bytes());

        // ImagingBoundingBox at 284..300
        buf[284..288].copy_from_slice(&self.imaging_bbox.0.to_be_bytes());
        buf[288..292].copy_from_slice(&self.imaging_bbox.1.to_be_bytes());
        buf[292..296].copy_from_slice(&self.imaging_bbox.2.to_be_bytes());
        buf[296..300].copy_from_slice(&self.imaging_bbox.3.to_be_bytes());

        // NumCopies at 340..344
        buf[340..344].copy_from_slice(&self.num_copies.to_be_bytes());

        // PageSize points at 352..360
        buf[352..356].copy_from_slice(&self.page_size_pts.0.to_be_bytes());
        buf[356..360].copy_from_slice(&self.page_size_pts.1.to_be_bytes());

        // Geometry at offsets 372..380
        buf[372..376].copy_from_slice(&self.width_px.to_be_bytes());
        buf[376..380].copy_from_slice(&self.height_px.to_be_bytes());

        // Color and depth at offsets 384..404
        buf[384..388].copy_from_slice(&self.bits_per_color.to_be_bytes());
        buf[388..392].copy_from_slice(&self.bits_per_pixel.to_be_bytes());
        buf[392..396].copy_from_slice(&self.bytes_per_line.to_be_bytes());
        buf[396..400].copy_from_slice(&0u32.to_be_bytes()); // Chunky color order
        buf[400..404].copy_from_slice(&self.color_space.to_be_bytes());

        // NumColors at 420..424
        buf[420..424].copy_from_slice(&self.num_colors.to_be_bytes());

        // TotalPageCount at 428..432 (or cupsInteger[0])
        buf[428..432].copy_from_slice(&self.total_pages.to_be_bytes());

        // cupsInteger[16] at 452..516
        for i in 0..16 {
            buf[452 + i * 4..456 + i * 4].copy_from_slice(&self.cups_integer[i].to_be_bytes());
        }

        // cupsRenderingIntent at 1668..1732
        let intent = self.rendering_intent.as_bytes();
        let intent_len = intent.len().min(63);
        buf[1668..1668 + intent_len].copy_from_slice(&intent[..intent_len]);

        // cupsPageSizeName at 1732..1796
        let name = self.page_size_name.as_bytes();
        let name_len = name.len().min(63);
        buf[1732..1732 + name_len].copy_from_slice(&name[..name_len]);

        buf
    }

    /// Parses a 1796-byte buffer into a `PwgRasterHeader`.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < PWG_HEADER_SIZE {
            return Err(ProtocolError::BufferTooShort {
                expected: PWG_HEADER_SIZE,
                actual: buf.len(),
            });
        }

        let media_class = String::from_utf8_lossy(&buf[0..64])
            .trim_matches('\0')
            .to_string();
        let media_type = String::from_utf8_lossy(&buf[128..192])
            .trim_matches('\0')
            .to_string();

        let dpi_x = u32::from_be_bytes(buf[276..280].try_into().unwrap());
        let dpi_y = u32::from_be_bytes(buf[280..284].try_into().unwrap());

        let imaging_bbox = (
            u32::from_be_bytes(buf[284..288].try_into().unwrap()),
            u32::from_be_bytes(buf[288..292].try_into().unwrap()),
            u32::from_be_bytes(buf[292..296].try_into().unwrap()),
            u32::from_be_bytes(buf[296..300].try_into().unwrap()),
        );

        let num_copies = u32::from_be_bytes(buf[340..344].try_into().unwrap());

        let page_size_pts = (
            u32::from_be_bytes(buf[352..356].try_into().unwrap()),
            u32::from_be_bytes(buf[356..360].try_into().unwrap()),
        );

        let width_px = u32::from_be_bytes(buf[372..376].try_into().unwrap());
        let height_px = u32::from_be_bytes(buf[376..380].try_into().unwrap());
        let bits_per_color = u32::from_be_bytes(buf[384..388].try_into().unwrap());
        let bits_per_pixel = u32::from_be_bytes(buf[388..392].try_into().unwrap());
        let bytes_per_line = u32::from_be_bytes(buf[392..396].try_into().unwrap());
        let color_space = u32::from_be_bytes(buf[400..404].try_into().unwrap());
        let num_colors = u32::from_be_bytes(buf[420..424].try_into().unwrap());
        let total_pages = u32::from_be_bytes(buf[428..432].try_into().unwrap());

        let mut cups_integer = [0u32; 16];
        for i in 0..16 {
            cups_integer[i] = u32::from_be_bytes(buf[452 + i * 4..456 + i * 4].try_into().unwrap());
        }

        let rendering_intent = String::from_utf8_lossy(&buf[1668..1732])
            .trim_matches('\0')
            .to_string();
        let page_size_name = String::from_utf8_lossy(&buf[1732..1796])
            .trim_matches('\0')
            .to_string();

        Ok(Self {
            media_class,
            media_type,
            dpi_x,
            dpi_y,
            imaging_bbox,
            num_copies,
            page_size_pts,
            width_px,
            height_px,
            bits_per_color,
            bits_per_pixel,
            bytes_per_line,
            color_space,
            num_colors,
            total_pages,
            cups_integer,
            rendering_intent,
            page_size_name,
        })
    }
}

/// Encodes a single validated scanline into PWG Raster PackBits compression.
///
/// Preceded by line repeat byte `0` (indicating the line is emitted once).
/// Pixel runs are compressed using $(N - 1)$ for identical runs and $(257 - K)$
/// for literal distinct runs.
pub fn encode_packbits_scanline(line: &ValidatedRasterLine) -> Vec<u8> {
    let mut out = Vec::new();
    out.push(0u8); // Repeat count = 0 (line printed 1 time)
    encode_pixel_runs(line.as_bytes(), line.bytes_per_pixel() as usize, &mut out);
    out
}

/// Encodes pixel runs within a scanline directly into `out` without repeat count byte.
fn encode_pixel_runs(bytes: &[u8], bytes_per_pixel: usize, out: &mut Vec<u8>) {
    let num_pixels = bytes.len() / bytes_per_pixel;
    if num_pixels == 0 {
        return;
    }

    let mut i = 0;
    while i < num_pixels {
        // Look ahead for run of identical pixels
        let cur_pixel = &bytes[i * bytes_per_pixel..(i + 1) * bytes_per_pixel];
        let mut run_len = 1;
        while (i + run_len) < num_pixels && run_len < 128 {
            let next_pixel =
                &bytes[(i + run_len) * bytes_per_pixel..(i + run_len + 1) * bytes_per_pixel];
            if next_pixel == cur_pixel {
                run_len += 1;
            } else {
                break;
            }
        }

        if run_len >= 2 || (run_len == 1 && i + 1 == num_pixels) {
            // Emit identical run: control byte is (N - 1)
            let control = (run_len - 1) as u8;
            out.push(control);
            out.extend_from_slice(cur_pixel);
            i += run_len;
        } else {
            // Find literal run of distinct pixels (up to 128)
            let lit_start = i;
            let mut lit_len = 0;
            while (i + lit_len) < num_pixels && lit_len < 128 {
                // Check if an identical run of >= 2 starts here
                if (i + lit_len + 1) < num_pixels {
                    let p1 = &bytes
                        [(i + lit_len) * bytes_per_pixel..(i + lit_len + 1) * bytes_per_pixel];
                    let p2 = &bytes
                        [(i + lit_len + 1) * bytes_per_pixel..(i + lit_len + 2) * bytes_per_pixel];
                    if p1 == p2 && lit_len >= 2 {
                        break;
                    }
                }
                lit_len += 1;
            }

            if lit_len == 1 {
                out.push(0); // 1 pixel identical run
                out.extend_from_slice(
                    &bytes[lit_start * bytes_per_pixel..(lit_start + 1) * bytes_per_pixel],
                );
            } else {
                // Literal distinct run of length K: control byte is (257 - K)
                let control = (257 - lit_len) as u8;
                out.push(control);
                out.extend_from_slice(
                    &bytes[lit_start * bytes_per_pixel..(lit_start + lit_len) * bytes_per_pixel],
                );
            }
            i += lit_len;
        }
    }
}

/// Decodes a PackBits-compressed scanline into raw pixel bytes.
///
/// Handles both raw compressed pixel streams and streams preceded by the PWG
/// line repeat count byte.
pub fn decode_packbits_scanline(
    compressed: &[u8],
    width_px: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let expected_len = width_px * bytes_per_pixel;

    // First try decoding skipping byte 0 (assuming repeat count byte was prepended)
    if compressed.len() > 1 {
        if let Ok(decoded) = decode_runs(&compressed[1..], expected_len, bytes_per_pixel) {
            if decoded.len() == expected_len {
                return Ok(decoded);
            }
        }
    }

    // Otherwise decode directly from byte 0
    decode_runs(compressed, expected_len, bytes_per_pixel)
}

fn decode_runs(
    mut stream: &[u8],
    expected_len: usize,
    bytes_per_pixel: usize,
) -> Result<Vec<u8>, ProtocolError> {
    let mut out = Vec::with_capacity(expected_len);

    while !stream.is_empty() && out.len() < expected_len {
        let control = stream[0];
        stream = &stream[1..];

        if control <= 127 {
            // Identical run of (control + 1) pixels
            let count = (control as usize) + 1;
            if stream.len() < bytes_per_pixel {
                return Err(ProtocolError::PackBitsOverrun);
            }
            let pixel = &stream[..bytes_per_pixel];
            stream = &stream[bytes_per_pixel..];
            for _ in 0..count {
                out.extend_from_slice(pixel);
            }
        } else if control >= 129 {
            // Literal distinct run of (257 - control) pixels
            let count = 257 - (control as usize);
            let needed_bytes = count * bytes_per_pixel;
            if stream.len() < needed_bytes {
                return Err(ProtocolError::PackBitsOverrun);
            }
            out.extend_from_slice(&stream[..needed_bytes]);
            stream = &stream[needed_bytes..];
        }
        // Control 128 is a no-op
    }

    if out.len() != expected_len {
        return Err(ProtocolError::PackBitsOverrun);
    }

    Ok(out)
}

/// Encodes an entire page into a complete PWG Raster stream (`RaS2` magic + header + compressed lines).
///
/// Optimizes consecutive identical scanlines using the PWG repeat count byte.
pub fn encode_pwg_page(header: &PwgRasterHeader, lines: &[ValidatedRasterLine]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&PWG_RASTER_MAGIC);
    out.extend_from_slice(&header.to_bytes());

    let bpp = (header.bits_per_pixel / 8) as usize;
    let mut y = 0;
    while y < lines.len() {
        // Count consecutive identical scanlines
        let current_line = &lines[y];
        let mut repeat_count = 0u8;
        while (y + (repeat_count as usize) + 1) < lines.len() && repeat_count < 255 {
            let next_line = &lines[y + (repeat_count as usize) + 1];
            if current_line.as_bytes() == next_line.as_bytes() {
                repeat_count += 1;
            } else {
                break;
            }
        }

        // Emit repeat byte followed by PackBits runs
        out.push(repeat_count);
        encode_pixel_runs(current_line.as_bytes(), bpp, &mut out);

        y += (repeat_count as usize) + 1;
    }

    out
}

/// 136-byte (`0x88`) frame header used for Canon CNCL IPC streaming.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CnclIpcFrameHeader {
    pub payload_size: u32,
    pub is_last_page: bool,
    pub page_number: u32,
    pub duplex_flag: u8,
    pub color_mode: u8,
}

impl CnclIpcFrameHeader {
    pub fn new(
        payload_size: u32,
        is_last_page: bool,
        page_number: u32,
        duplex_flag: u8,
        color_mode: u8,
    ) -> Self {
        Self {
            payload_size,
            is_last_page,
            page_number,
            duplex_flag,
            color_mode,
        }
    }

    /// Serializes this frame into a 136-byte buffer.
    pub fn to_bytes(&self) -> [u8; CNCL_IPC_FRAME_SIZE] {
        let mut buf = [0u8; CNCL_IPC_FRAME_SIZE];
        buf[0..4].copy_from_slice(&CNCL_IPC_MAGIC.to_be_bytes());
        buf[4..8].copy_from_slice(&self.payload_size.to_be_bytes());
        buf[8..12].copy_from_slice(&self.page_number.to_be_bytes());
        buf[12] = if self.is_last_page { 1 } else { 0 };
        buf[13] = self.duplex_flag;
        buf[14] = self.color_mode;
        buf
    }

    /// Deserializes a 136-byte buffer into a `CnclIpcFrameHeader`.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, ProtocolError> {
        if buf.len() < CNCL_IPC_FRAME_SIZE {
            return Err(ProtocolError::BufferTooShort {
                expected: CNCL_IPC_FRAME_SIZE,
                actual: buf.len(),
            });
        }

        let magic = u32::from_be_bytes(buf[0..4].try_into().unwrap());
        if magic != CNCL_IPC_MAGIC {
            return Err(ProtocolError::InvalidMagic {
                expected: CNCL_IPC_MAGIC.to_be_bytes(),
                actual: buf[0..4].try_into().unwrap(),
            });
        }

        let payload_size = u32::from_be_bytes(buf[4..8].try_into().unwrap());
        let page_number = u32::from_be_bytes(buf[8..12].try_into().unwrap());
        let is_last_page = buf[12] != 0;
        let duplex_flag = buf[13];
        let color_mode = buf[14];

        Ok(Self {
            payload_size,
            is_last_page,
            page_number,
            duplex_flag,
            color_mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pwg_header_round_trip_srgb() {
        let header = PwgRasterHeader::new_srgb(5100, 7000, 600);
        let bytes = header.to_bytes();
        assert_eq!(bytes.len(), PWG_HEADER_SIZE);
        assert_eq!(&bytes[0..9], b"PwgRaster");

        let parsed = PwgRasterHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.dpi_x, 600);
        assert_eq!(parsed.dpi_y, 600);
        assert_eq!(parsed.width_px, 5100);
        assert_eq!(parsed.height_px, 7000);
        assert_eq!(parsed.bits_per_pixel, 24);
        assert_eq!(parsed.color_space, 19);
        assert_eq!(parsed.num_colors, 3);
    }

    #[test]
    fn pwg_header_round_trip_sgray() {
        let header = PwgRasterHeader::new_sgray(2550, 3500, 300);
        let bytes = header.to_bytes();
        let parsed = PwgRasterHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.dpi_x, 300);
        assert_eq!(parsed.bits_per_pixel, 8);
        assert_eq!(parsed.color_space, 18);
        assert_eq!(parsed.num_colors, 1);
    }

    #[test]
    fn packbits_scanline_round_trip_srgb() {
        let width = 64;
        let mut pixels = Vec::with_capacity(width * 3);
        // Create 32 red pixels, then 32 distinct pixels
        for _ in 0..32 {
            pixels.extend_from_slice(&[255, 0, 0]);
        }
        for i in 0..32 {
            pixels.extend_from_slice(&[i as u8, (i * 2) as u8, (i * 3) as u8]);
        }

        let line = ValidatedRasterLine::try_new(pixels.clone(), width as u32, 600, 3).unwrap();
        let compressed = encode_packbits_scanline(&line);
        let decoded = decode_packbits_scanline(&compressed, width, 3).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn packbits_scanline_round_trip_sgray() {
        let width = 100;
        let mut pixels = vec![128u8; 50];
        pixels.extend(vec![255u8; 50]);

        let line = ValidatedRasterLine::try_new(pixels.clone(), width as u32, 300, 1).unwrap();
        let compressed = encode_packbits_scanline(&line);
        let decoded = decode_packbits_scanline(&compressed, width, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_pwg_page_with_magic_and_repeats() {
        let header = PwgRasterHeader::new_srgb(10, 3, 600);
        let line1 = ValidatedRasterLine::try_new(vec![255; 30], 10, 600, 3).unwrap();
        let line2 = line1.clone();
        let line3 = ValidatedRasterLine::try_new(vec![0; 30], 10, 600, 3).unwrap();

        let stream = encode_pwg_page(&header, &[line1, line2, line3]);
        assert_eq!(&stream[0..4], &PWG_RASTER_MAGIC);
        let header_bytes = header.to_bytes();
        assert_eq!(&stream[4..4 + PWG_HEADER_SIZE], &header_bytes[..]);
        // After header: repeat count byte for line1 and line2 should be 1 (2 lines)
        assert_eq!(stream[4 + PWG_HEADER_SIZE], 1);
    }

    #[test]
    fn cncl_ipc_frame_round_trip() {
        let frame = CnclIpcFrameHeader::new(65536, true, 1, 0, 1);
        let bytes = frame.to_bytes();
        assert_eq!(bytes.len(), CNCL_IPC_FRAME_SIZE);

        let parsed = CnclIpcFrameHeader::from_bytes(&bytes).unwrap();
        assert_eq!(parsed.payload_size, 65536);
        assert!(parsed.is_last_page);
        assert_eq!(parsed.page_number, 1);
        assert_eq!(parsed.duplex_flag, 0);
        assert_eq!(parsed.color_mode, 1);
    }

    #[test]
    fn cncl_ipc_frame_rejects_bad_magic() {
        let mut bytes = [0u8; 136];
        bytes[0..4].copy_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        assert!(matches!(
            CnclIpcFrameHeader::from_bytes(&bytes),
            Err(ProtocolError::InvalidMagic { .. })
        ));
    }
}
