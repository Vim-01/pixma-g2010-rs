//! Canon ESC/P BJRaster3 protocol encoder.
//!
//! Generates low-level BJRaster3 command sequences directly accepted by Canon
//! printer firmware over USB Interface 1 Bulk OUT.
//!
//! Enforces compile-time and runtime mechanical limits via [`ValidatedRasterLine`].

use crate::errors::ProtocolError;
use pixma_core::errors::HardwareConstraintViolation;
use pixma_core::print_params::{InkDryingTime, MediaType, PaperSize, PrintJobConfig};
use pixma_core::safety::ValidatedRasterLine;

/// Form Feed ASCII byte (`0x0C` / `\f`).
pub const FORM_FEED: u8 = 0x0C;

/// Canon ESC/P BJRaster3 command generator.
pub struct BjRaster3Encoder;

impl BjRaster3Encoder {
    /// Generates `ESC ( c`: Color vs Monochrome mode command.
    ///
    /// Sequence: `\x1b(c\x03\x00\x00<color_flag>\x00`
    pub fn encode_color_mode(color: bool) -> Vec<u8> {
        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&[0x1b, b'(', b'c', 0x03, 0x00]);
        buf.push(0x00);
        buf.push(if color { 0x01 } else { 0x00 });
        buf.push(0x00);
        buf
    }

    /// Generates `ESC ( t`: Base print resolution command.
    ///
    /// Sequence: `\x1b(t\x03\x00\x00<dpi_code_hi><dpi_code_lo>`
    pub fn encode_resolution(dpi: u32) -> Result<Vec<u8>, ProtocolError> {
        let (hi, lo) = match dpi {
            300 => (0x01, 0x2C),  // 300
            600 => (0x02, 0x58),  // 600
            1200 => (0x04, 0xB0), // 1200
            other => {
                return Err(HardwareConstraintViolation::UnsupportedResolutionDpi(other).into())
            }
        };

        let mut buf = Vec::with_capacity(8);
        buf.extend_from_slice(&[0x1b, b'(', b't', 0x03, 0x00]);
        buf.push(0x00);
        buf.push(hi);
        buf.push(lo);
        Ok(buf)
    }

    /// Generates `ESC ( l`: Page length in lines.
    ///
    /// Sequence: `\x1b(l\x04\x00<lines_le32>`
    pub fn encode_page_length(lines: u32) -> Vec<u8> {
        let mut buf = Vec::with_capacity(9);
        buf.extend_from_slice(&[0x1b, b'(', b'l', 0x04, 0x00]);
        buf.extend_from_slice(&lines.to_le_bytes());
        buf
    }

    /// Generates `ESC ( p`: Paper format and physical paper size.
    ///
    /// Sequence: `\x1b(p\x04\x00<width_mm_lo><width_mm_hi><height_mm_lo><height_mm_hi>`
    pub fn encode_paper_size(paper_size: PaperSize) -> Vec<u8> {
        let (w_mm, h_mm) = paper_size.dimensions_mm();
        let w_u16 = (w_mm * 10.0) as u16;
        let h_u16 = (h_mm * 10.0) as u16;

        let mut buf = Vec::with_capacity(9);
        buf.extend_from_slice(&[0x1b, b'(', b'p', 0x04, 0x00]);
        buf.extend_from_slice(&w_u16.to_le_bytes());
        buf.extend_from_slice(&h_u16.to_le_bytes());
        buf
    }

    /// Generates `ESC ( M`: Media type selection code.
    pub fn encode_media_type(media_type: MediaType) -> Vec<u8> {
        let code = match media_type {
            MediaType::Plain => 0x01,
            MediaType::PhotoPlusGlossyII => 0x06,
            MediaType::PhotoProPlatinum => 0x03,
            MediaType::PhotoProLuster => 0x11,
            MediaType::PhotoPlusSemiGloss => 0x09,
            MediaType::GlossyPhoto => 0x02,
            MediaType::MattePhoto => 0x0B,
            MediaType::HighResolutionPaper => 0x12,
            MediaType::Envelope => 0x0C,
            MediaType::InkjetHagaki => 0x0E,
            MediaType::Hagaki => 0x0F,
            MediaType::TShirtTransfer | MediaType::Cardstock => 0x12,
        };

        let mut buf = Vec::with_capacity(6);
        buf.extend_from_slice(&[0x1b, b'(', b'M', 0x01, 0x00]);
        buf.push(code);
        buf
    }

    /// Generates `ESC ( T`: Ink drying wait time.
    pub fn encode_ink_drying_time(drying_time: InkDryingTime) -> Vec<u8> {
        let secs = drying_time.pause_seconds() as u8;
        let mut buf = Vec::with_capacity(6);
        buf.extend_from_slice(&[0x1b, b'(', b'T', 0x01, 0x00]);
        buf.push(secs);
        buf
    }

    /// Generates `ESC ( d`: Compressed or raw raster data band.
    ///
    /// Strictly takes [`ValidatedRasterLine`], guaranteeing that carriage travel
    /// dot limits and buffer lengths have been verified.
    ///
    /// Sequence: `\x1b(d<len_lo><len_hi><plane_id><compression_mode><payload>`
    pub fn encode_raster_band(plane: u8, line: &ValidatedRasterLine) -> Vec<u8> {
        let raw = line.as_bytes();
        let compressed = packbits_compress(raw);

        // Header length is 2 bytes (plane_id + compression_mode) + payload length
        let payload_len = compressed.len();
        let total_cmd_len = (2 + payload_len) as u16;

        let mut buf = Vec::with_capacity(5 + 2 + payload_len);
        buf.extend_from_slice(&[0x1b, b'(', b'd']);
        buf.extend_from_slice(&total_cmd_len.to_le_bytes());
        buf.push(plane);
        buf.push(0x01); // 0x01 = PackBits / TIFF RLE
        buf.extend_from_slice(&compressed);
        buf
    }

    /// Generates form feed page eject (`0x0C`).
    pub const fn encode_form_feed() -> [u8; 1] {
        [FORM_FEED]
    }

    /// Generates the complete job/page prologue from a `PrintJobConfig`.
    pub fn encode_job_prologue(config: &PrintJobConfig) -> Result<Vec<u8>, ProtocolError> {
        let mut buf = Vec::new();
        buf.extend(Self::encode_color_mode(!config.color.grayscale));
        buf.extend(Self::encode_resolution(config.quality.base_raster_dpi())?);
        buf.extend(Self::encode_media_type(config.media_type));
        buf.extend(Self::encode_paper_size(config.paper_size));
        buf.extend(Self::encode_ink_drying_time(config.drying_time));
        Ok(buf)
    }

    /// Encodes a complete page of validated raster lines into a BJRaster3 stream.
    pub fn encode_page(
        config: &PrintJobConfig,
        lines: &[ValidatedRasterLine],
    ) -> Result<Vec<u8>, ProtocolError> {
        let mut buf = Self::encode_job_prologue(config)?;
        buf.extend(Self::encode_page_length(lines.len() as u32));

        let expected_dpi = config.quality.base_raster_dpi();
        for line in lines {
            if line.dpi() != expected_dpi {
                return Err(
                    HardwareConstraintViolation::UnsupportedResolutionDpi(line.dpi()).into(),
                );
            }
            buf.extend(Self::encode_raster_band(0x00, line));
        }

        buf.push(FORM_FEED);
        Ok(buf)
    }

    // -----------------------------------------------------------------------
    // Canon BJR520 (`CA_BJR520_IJP` — G1000/G1010/G2000/G2010) Hardware Encoder
    // -----------------------------------------------------------------------

    /// Generates the hardware-verified BJR520 page prologue (`ESC [ K` .. `ESC ( L`)
    /// accepted by physical Canon PIXMA G2010 (`CID:CA_BJR520_IJP`) firmware.
    pub fn encode_bjr520_prologue(
        config: &PrintJobConfig,
        page_no: u8,
        printable_width_600: u32,
        printable_height_600: u32,
    ) -> Vec<u8> {
        let mut buf = Vec::with_capacity(180);

        if page_no <= 1 {
            // ESC [ K (len=2): 00 0f — Enter BJRaster3 command mode
            buf.extend_from_slice(&[0x1b, b'[', b'K', 0x02, 0x00, 0x00, 0x0f]);
            // ESC ( b (len=1): 01 — Enable PackBits RLE compression
            buf.extend_from_slice(&[0x1b, b'(', b'b', 0x01, 0x00, 0x01]);
        }

        // ESC ( q (len=1): page ID
        buf.extend_from_slice(&[0x1b, b'(', b'q', 0x01, 0x00, page_no.max(1)]);

        // ESC ( d (len=4): 02 58 02 58 (600 x 600 DPI in big-endian u16)
        buf.extend_from_slice(&[0x1b, b'(', b'd', 0x04, 0x00, 0x02, 0x58, 0x02, 0x58]);

        // ESC ( t (len=24): Image bit-depth and level descriptor table
        // Hardware-fixed descriptor table for CA_BJR520_IJP firmware:
        // Slot 0 (K / BH-4 head): 1-bpp, 2 levels (01 00 02)
        // Slot 1 (C / CH-41 head): 2-bpp, 3 levels (02 00 03)
        // Slot 2 (M / CH-41 head): 2-bpp, 3 levels (02 00 03)
        // Slot 3 (Y / CH-41 head): 1-bpp, 2 levels (01 00 02)
        buf.extend_from_slice(&[
            0x1b, b'(', b't', 0x18, 0x00, 0x80, 0x80, 0x01, 0x01, 0x00, 0x02, 0x02, 0x00, 0x03,
            0x02, 0x00, 0x03, 0x01, 0x00, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00,
        ]);

        // ESC ( c (len=3): Color/Monochrome + media + quality
        let color_byte = if config.color.grayscale { 0x31 } else { 0x30 };
        buf.extend_from_slice(&[0x1b, b'(', b'c', 0x03, 0x00, color_byte, 0x00, 0x02]);

        // ESC ( p (len=46): Extended printable area and physical paper geometry in 1/600 inch
        let (w_mm, h_mm) = config.paper_size.dimensions_mm();
        let (phys_w_600, phys_h_600, pw_60, ph_60) = match config.paper_size {
            PaperSize::A4 => (4961u32, 7016u32, 480u16, 682u16),
            PaperSize::Letter => (5100u32, 6600u32, 494u16, 640u16),
            PaperSize::Photo4x6 => (2400u32, 3600u32, 224u16, 340u16),
            _ => {
                let pw = ((w_mm / 25.4) * 600.0).round() as u32;
                let ph = ((h_mm / 25.4) * 600.0).round() as u32;
                (
                    pw,
                    ph,
                    (printable_width_600 / 10) as u16,
                    (printable_height_600 / 10) as u16,
                )
            }
        };
        let left_margin_600: u32 = 83;
        let top_margin_600: u32 = 75;

        buf.extend_from_slice(&[0x1b, b'(', b'p', 0x2e, 0x00]);
        buf.extend_from_slice(&ph_60.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&pw_60.to_be_bytes());
        buf.extend_from_slice(&0u16.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&600u16.to_be_bytes());
        buf.extend_from_slice(&left_margin_600.to_be_bytes());
        buf.extend_from_slice(&top_margin_600.to_be_bytes());
        buf.extend_from_slice(&printable_width_600.to_be_bytes());
        buf.extend_from_slice(&printable_height_600.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&0u32.to_be_bytes());
        buf.extend_from_slice(&phys_w_600.to_be_bytes());
        buf.extend_from_slice(&phys_h_600.to_be_bytes());

        // ESC ( P (len=9): Paper size code and print parameters
        let paper_code: u8 = match config.paper_size {
            PaperSize::A4 => 0x03,
            PaperSize::Letter => 0x0d,
            PaperSize::Legal => 0x0f,
            PaperSize::A5 => 0x05,
            PaperSize::B5 => 0x08,
            PaperSize::Photo4x6 => 0x00,
            PaperSize::Photo5x7 => 0x15,
            _ => 0x03,
        };
        buf.extend_from_slice(&[
            0x1b, b'(', b'P', 0x09, 0x00, 0x00, paper_code, 0x00, 0x00, 0x01, 0x00, 0x01, 0x00,
            0xff,
        ]);

        // ESC ( l (len=3): Rear paper tray selection (0x34)
        buf.extend_from_slice(&[0x1b, b'(', b'l', 0x03, 0x00, 0x34, 0x00, 0x00]);

        // ESC ( v (len=1): 00
        buf.extend_from_slice(&[0x1b, b'(', b'v', 0x01, 0x00, 0x00]);

        // ESC ( I (len=1): 01 — Multi-scanline block mode
        buf.extend_from_slice(&[0x1b, b'(', b'I', 0x01, 0x00, 0x01]);

        // ESC ( J (len=1): 08 — 8 scanlines per ESC ( F block
        buf.extend_from_slice(&[0x1b, b'(', b'J', 0x01, 0x00, 0x08]);

        // ESC ( L: Plane order ("K" for monochrome, "CMYK" for color)
        if config.color.grayscale {
            buf.extend_from_slice(&[0x1b, b'(', b'L', 0x01, 0x00, b'K']);
        } else {
            buf.extend_from_slice(&[0x1b, b'(', b'L', 0x04, 0x00, b'C', b'M', b'Y', b'K']);
        }

        buf
    }

    /// Encodes up to 8 packed bitplane scanlines into a single `ESC ( F` multi-scanline block,
    /// where each scanline is PackBits-compressed (`0x00..=0x7f` literal, `0x81..=0xff` repeat)
    /// and terminated by the `0x80` end-of-scanline marker.
    pub fn encode_bjr520_band_esc_f(lines: &[&[u8]]) -> Vec<u8> {
        let mut payload = Vec::with_capacity(64);
        for row in 0..8 {
            if let Some(line) = lines.get(row) {
                if !line.iter().all(|&b| b == 0) {
                    packbits_compress_into(line, &mut payload);
                }
            }
            // 0x80 marks the end of each of the 8 scanlines inside ESC ( F
            payload.push(0x80);
        }

        let len = payload.len() as u16;
        let mut out = Vec::with_capacity(5 + payload.len());
        out.extend_from_slice(&[0x1b, b'(', b'F']);
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&payload);
        out
    }

    /// Generates the BJR520 page/job epilogue (`FF`, and on the final page `ESC ( b 0` + `ESC @`).
    pub fn encode_bjr520_epilogue(is_last_page: bool) -> Vec<u8> {
        let mut buf = vec![FORM_FEED];
        if is_last_page {
            buf.extend_from_slice(&[0x1b, b'(', b'b', 0x01, 0x00, 0x00]);
            buf.extend_from_slice(&[0x1b, b'@']);
        }
        buf
    }
}

/// Appends PackBits-compressed bytes of `data` into `out` (guaranteeing `0x80` is never emitted).
fn packbits_compress_into(data: &[u8], out: &mut Vec<u8>) {
    out.extend(packbits_compress(data));
}

/// Standard TIFF / ESC/P PackBits byte compression.
fn packbits_compress(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;

    while i < data.len() {
        // Look for run of identical bytes
        let cur = data[i];
        let mut run_len = 1;
        while (i + run_len) < data.len() && run_len < 128 && data[i + run_len] == cur {
            run_len += 1;
        }

        if run_len >= 3 || (run_len >= 1 && (i + run_len == data.len())) {
            if run_len == 1 {
                // Single literal byte
                out.push(0); // literal length 1
                out.push(cur);
            } else {
                // Identical run of run_len: control byte is (257 - run_len) in standard TIFF PackBits
                // or (1 - run_len) as signed i8.
                let control = (257 - run_len) as u8;
                out.push(control);
                out.push(cur);
            }
            i += run_len;
        } else {
            // Literal run of distinct bytes
            let lit_start = i;
            let mut lit_len = 0;
            while (i + lit_len) < data.len() && lit_len < 128 {
                if (i + lit_len + 2) < data.len()
                    && data[i + lit_len] == data[i + lit_len + 1]
                    && data[i + lit_len] == data[i + lit_len + 2]
                {
                    break;
                }
                lit_len += 1;
            }

            if lit_len == 1 {
                out.push(0);
                out.push(data[lit_start]);
            } else {
                let control = (lit_len - 1) as u8;
                out.push(control);
                out.extend_from_slice(&data[lit_start..lit_start + lit_len]);
            }
            i += lit_len;
        }
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::safety::ValidatedRasterLine;

    #[test]
    fn encodes_bjraster3_commands() {
        let color = BjRaster3Encoder::encode_color_mode(true);
        assert_eq!(&color[0..3], &[0x1b, b'(', b'c']);

        let res = BjRaster3Encoder::encode_resolution(600).unwrap();
        assert_eq!(&res[0..3], &[0x1b, b'(', b't']);
        assert_eq!(&res[5..8], &[0x00, 0x02, 0x58]);

        let lines = BjRaster3Encoder::encode_page_length(1000);
        assert_eq!(&lines[0..3], &[0x1b, b'(', b'l']);
        assert_eq!(&lines[5..9], &1000u32.to_le_bytes());

        let ff = BjRaster3Encoder::encode_form_feed();
        assert_eq!(ff, [0x0C]);
    }

    #[test]
    fn encodes_raster_band_with_validated_line() {
        let line = ValidatedRasterLine::try_new(vec![0xAA; 300], 100, 600, 3).unwrap();
        let band = BjRaster3Encoder::encode_raster_band(0x01, &line);
        assert_eq!(&band[0..3], &[0x1b, b'(', b'd']);
        assert_eq!(band[5], 0x01); // plane
        assert_eq!(band[6], 0x01); // compression
    }

    #[test]
    fn encodes_complete_page() {
        let config = PrintJobConfig::default();
        let line = ValidatedRasterLine::try_new(vec![0xFF; 300], 100, 600, 3).unwrap();
        let page = BjRaster3Encoder::encode_page(&config, &[line]).unwrap();

        assert!(page.starts_with(&[0x1b, b'(', b'c']));
        assert_eq!(page.last(), Some(&FORM_FEED));
    }
}
