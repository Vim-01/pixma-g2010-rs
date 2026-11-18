//! Ingestion of CUPS / PWG Raster v2 streams, standard images, and synthetic test pages.
//!
//! Provides [`RasterPageInput`], representing an uncompressed 24-bit sRGB raster page
//! with discrete physical dimensions and resolution, along with parsers for standard
//! CUPS raster streams (`"RaS2"` magic) and image formats (PNG, JPEG, TIFF, WebP).

use crate::errors::RasterError;
use pixma_core::print_params::PaperSize;
use pixma_core::safety::MechanicalBounds;
use pixma_protocol::pwg_raster::{
    PwgRasterHeader, CUPS_RASTER_REVSYNC_V2, CUPS_RASTER_REVSYNC_V3, CUPS_RASTER_SYNC_V3,
    PWG_HEADER_SIZE, PWG_RASTER_MAGIC,
};
use std::io::{BufRead, BufReader, Cursor, Read, Write};
use std::path::PathBuf;

/// An uncompressed 24-bit sRGB raster page ready for layout, color processing, and halftoning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RasterPageInput {
    /// Horizontal pixel count.
    pub width: u32,
    /// Vertical pixel count.
    pub height: u32,
    /// Resolution in dots per inch.
    pub dpi: u32,
    /// Interleaved sRGB pixel bytes `[R, G, B, R, G, B, ...]`.
    /// Length must equal `width * height * 3`.
    pub rgb_pixels: Vec<u8>,
}

impl RasterPageInput {
    /// Constructs and validates a new raster page input.
    pub fn new(
        width: u32,
        height: u32,
        dpi: u32,
        rgb_pixels: Vec<u8>,
    ) -> Result<Self, RasterError> {
        if width == 0 || height == 0 {
            return Err(RasterError::InvalidDimensions { width, height });
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|px| px.checked_mul(3))
            .ok_or(RasterError::InvalidDimensions { width, height })?;

        if rgb_pixels.len() != expected {
            return Err(RasterError::BufferLengthMismatch {
                expected,
                actual: rgb_pixels.len(),
            });
        }

        Ok(Self {
            width,
            height,
            dpi: if dpi == 0 { 300 } else { dpi },
            rgb_pixels,
        })
    }

    /// Constructs a blank white page (`[255, 255, 255]`) of the specified dimensions.
    pub fn new_blank(width: u32, height: u32, dpi: u32) -> Self {
        let size = (width as usize)
            .saturating_mul(height as usize)
            .saturating_mul(3);
        Self {
            width,
            height,
            dpi: if dpi == 0 { 300 } else { dpi },
            rgb_pixels: vec![255u8; size],
        }
    }

    /// Returns the pixel width.
    #[inline]
    pub const fn width(&self) -> u32 {
        self.width
    }

    /// Returns the pixel height.
    #[inline]
    pub const fn height(&self) -> u32 {
        self.height
    }

    /// Returns the dots-per-inch resolution.
    #[inline]
    pub const fn dpi(&self) -> u32 {
        self.dpi
    }

    /// Returns a reference to the raw interleaved sRGB pixel slice.
    #[inline]
    pub fn rgb_pixels(&self) -> &[u8] {
        &self.rgb_pixels
    }

    /// Returns a mutable reference to the raw interleaved sRGB pixel slice.
    #[inline]
    pub fn rgb_pixels_mut(&mut self) -> &mut [u8] {
        &mut self.rgb_pixels
    }

    /// Samples an RGB pixel at coordinate `(x, y)`.
    #[inline]
    pub fn get_pixel(&self, x: u32, y: u32) -> Option<[u8; 3]> {
        if x >= self.width || y >= self.height {
            return None;
        }
        let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
        Some([
            self.rgb_pixels[idx],
            self.rgb_pixels[idx + 1],
            self.rgb_pixels[idx + 2],
        ])
    }

    /// Sets an RGB pixel at coordinate `(x, y)`.
    #[inline]
    pub fn set_pixel(&mut self, x: u32, y: u32, rgb: [u8; 3]) {
        if x < self.width && y < self.height {
            let idx = ((y as usize) * (self.width as usize) + (x as usize)) * 3;
            self.rgb_pixels[idx] = rgb[0];
            self.rgb_pixels[idx + 1] = rgb[1];
            self.rgb_pixels[idx + 2] = rgb[2];
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CupsRasterFormat {
    Compressed { little_endian: bool },
    Uncompressed { little_endian: bool },
}

fn detect_cups_raster_magic(magic: &[u8]) -> Option<CupsRasterFormat> {
    if magic.len() < 4 {
        return None;
    }
    match [magic[0], magic[1], magic[2], magic[3]] {
        m if m == PWG_RASTER_MAGIC => Some(CupsRasterFormat::Compressed {
            little_endian: false,
        }),
        m if m == CUPS_RASTER_REVSYNC_V2 => Some(CupsRasterFormat::Compressed {
            little_endian: true,
        }),
        m if m == CUPS_RASTER_SYNC_V3 => Some(CupsRasterFormat::Uncompressed {
            little_endian: false,
        }),
        m if m == CUPS_RASTER_REVSYNC_V3 => Some(CupsRasterFormat::Uncompressed {
            little_endian: true,
        }),
        _ => None,
    }
}

fn convert_scanline_to_rgb(
    line_bytes: &[u8],
    width: u32,
    header: &PwgRasterHeader,
    little_endian: bool,
    rgb_out: &mut Vec<u8>,
) -> Result<(), RasterError> {
    let w = width as usize;
    match (header.bits_per_pixel, header.bits_per_color) {
        (1, _) => {
            let needed = w.div_ceil(8);
            if line_bytes.len() < needed {
                return Err(RasterError::StreamParse(
                    "Truncated 1-bit CUPS raster scanline".to_string(),
                ));
            }
            // CUPS_CSPACE_K = 3 (1 means black ink), W/SW = 0/18 (1 means white)
            let is_k = header.color_space == 3;
            for x in 0..w {
                let bit = (line_bytes[x / 8] >> (7 - (x % 8))) & 1;
                let v = if is_k {
                    if bit == 1 {
                        0
                    } else {
                        255
                    }
                } else if bit == 1 {
                    255
                } else {
                    0
                };
                rgb_out.extend_from_slice(&[v, v, v]);
            }
        }
        (8, 8) | (8, 0) => {
            if line_bytes.len() < w {
                return Err(RasterError::StreamParse(
                    "Truncated 8-bit CUPS raster scanline".to_string(),
                ));
            }
            let is_k = header.color_space == 3;
            for &g in &line_bytes[..w] {
                let val = if is_k { 255u8.saturating_sub(g) } else { g };
                rgb_out.extend_from_slice(&[val, val, val]);
            }
        }
        (16, 16) => {
            let needed = w * 2;
            if line_bytes.len() < needed {
                return Err(RasterError::StreamParse(
                    "Truncated 16-bit gray CUPS raster scanline".to_string(),
                ));
            }
            let is_k = header.color_space == 3;
            let msb_idx = usize::from(little_endian);
            for chunk in line_bytes[..needed].as_chunks::<2>().0 {
                let val = if is_k {
                    255u8.saturating_sub(chunk[msb_idx])
                } else {
                    chunk[msb_idx]
                };
                rgb_out.extend_from_slice(&[val, val, val]);
            }
        }
        (24, _) => {
            let needed = w * 3;
            if line_bytes.len() < needed {
                return Err(RasterError::StreamParse(
                    "Truncated 24-bit RGB CUPS raster scanline".to_string(),
                ));
            }
            if header.color_space == 4 {
                // CUPS_CSPACE_CMY -> subtractive sRGB
                for chunk in line_bytes[..needed].as_chunks::<3>().0 {
                    rgb_out.extend_from_slice(&[
                        255u8.saturating_sub(chunk[0]),
                        255u8.saturating_sub(chunk[1]),
                        255u8.saturating_sub(chunk[2]),
                    ]);
                }
            } else {
                rgb_out.extend_from_slice(&line_bytes[..needed]);
            }
        }
        (32, _) => {
            let needed = w * 4;
            if line_bytes.len() < needed {
                return Err(RasterError::StreamParse(
                    "Truncated 32-bit CUPS raster scanline".to_string(),
                ));
            }
            if header.color_space == 6 {
                // CMYK -> subtractive sRGB
                for chunk in line_bytes[..needed].as_chunks::<4>().0 {
                    let c = chunk[0] as u16;
                    let m = chunk[1] as u16;
                    let y = chunk[2] as u16;
                    let k = chunk[3] as u16;
                    rgb_out.extend_from_slice(&[
                        255u8.saturating_sub((c + k).min(255) as u8),
                        255u8.saturating_sub((m + k).min(255) as u8),
                        255u8.saturating_sub((y + k).min(255) as u8),
                    ]);
                }
            } else {
                // RGBA / RGBW -> drop 4th channel
                for chunk in line_bytes[..needed].as_chunks::<4>().0 {
                    rgb_out.extend_from_slice(&[chunk[0], chunk[1], chunk[2]]);
                }
            }
        }
        (48, 16) => {
            let needed = w * 6;
            if line_bytes.len() < needed {
                return Err(RasterError::StreamParse(
                    "Truncated 48-bit RGB CUPS raster scanline".to_string(),
                ));
            }
            let msb = usize::from(little_endian);
            for chunk in line_bytes[..needed].as_chunks::<6>().0 {
                rgb_out.extend_from_slice(&[chunk[msb], chunk[2 + msb], chunk[4 + msb]]);
            }
        }
        (other_bpp, other_bpc) => {
            return Err(RasterError::StreamParse(format!(
                "Unsupported CUPS raster depth: {other_bpp} bpp ({other_bpc} bpc)"
            )));
        }
    }
    Ok(())
}

const MAX_PWG_WIDTH_PX: u32 = 16_384;
const MAX_PWG_HEIGHT_PX: u32 = 32_768;
const MAX_PWG_RGB_BYTES: usize = 512 * 1024 * 1024;
const MAX_PWG_TOTAL_RGB_BYTES: usize = 512 * 1024 * 1024;
/// Upper bound on total buffered RGB bytes when collecting multi-page documents
/// that require random page access (N-Up, Booklet, Reverse Order, Collated Copies).
pub const MAX_MULTI_PAGE_COLLECT_RGB_BYTES: usize = 2 * 1024 * 1024 * 1024;
const MAX_PWG_PAGES: usize = 256;
const MAX_PWG_LINE_BYTES: usize = 1024 * 1024;
const MAX_PWG_EAGER_PREALLOC_BYTES: usize = 16 * 1024 * 1024;

fn read_prefix<R: Read + ?Sized>(reader: &mut R, max_len: usize) -> Result<Vec<u8>, RasterError> {
    let mut buf = Vec::with_capacity(max_len);
    while buf.len() < max_len {
        let mut chunk = [0u8; 64];
        let want = (max_len - buf.len()).min(chunk.len());
        match reader.read(&mut chunk[..want]) {
            Ok(0) => break,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(RasterError::Io(e)),
        }
    }
    Ok(buf)
}

fn read_exact_stream<R: Read + ?Sized>(
    reader: &mut R,
    dst: &mut [u8],
    eof_msg: &str,
) -> Result<(), RasterError> {
    match reader.read_exact(dst) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
            Err(RasterError::StreamParse(eof_msg.to_string()))
        }
        Err(e) => Err(RasterError::Io(e)),
    }
}

fn read_u8_stream<R: BufRead + ?Sized>(reader: &mut R) -> Result<Option<u8>, RasterError> {
    loop {
        match reader.fill_buf() {
            Ok([]) => return Ok(None),
            Ok(buf) => {
                let b = buf[0];
                reader.consume(1);
                return Ok(Some(b));
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(RasterError::Io(e)),
        }
    }
}

/// Streaming CUPS / PWG Raster (`RaS2`/`2SaR`/`RaS3`/`3SaR`) decoder yielding one page at a time.
pub struct PwgRasterReader<R: BufRead> {
    reader: R,
    little_endian: bool,
    compressed: bool,
    pages_emitted: usize,
    max_pages: usize,
}

impl<R: BufRead> PwgRasterReader<R> {
    /// Opens a CUPS / PWG Raster stream and validates its 4-byte stream magic prefix.
    pub fn new(reader: R) -> Result<Self, RasterError> {
        Self::with_max_pages(reader, MAX_PWG_PAGES)
    }

    fn with_max_pages(mut reader: R, max_pages: usize) -> Result<Self, RasterError> {
        let magic = read_prefix(&mut reader, 4)?;
        if magic.len() < 4 {
            return Err(RasterError::StreamParse(format!(
                "Stream too short for CUPS/PWG Raster: length is {} bytes",
                magic.len()
            )));
        }
        let format = detect_cups_raster_magic(&magic).ok_or_else(|| {
            RasterError::StreamParse("Missing CUPS/PWG Raster magic prefix".to_string())
        })?;
        let (little_endian, compressed) = match format {
            CupsRasterFormat::Compressed { little_endian } => (little_endian, true),
            CupsRasterFormat::Uncompressed { little_endian } => (little_endian, false),
        };
        Ok(Self {
            reader,
            little_endian,
            compressed,
            pages_emitted: 0,
            max_pages,
        })
    }

    /// Reads and decodes the next raster page from the stream (`Ok(None)` at clean EOF).
    pub fn next_page(&mut self) -> Result<Option<RasterPageInput>, RasterError> {
        let mut dummy = 0usize;
        self.next_page_with_budget(None, &mut dummy)
    }

    fn next_page_with_budget(
        &mut self,
        max_cumulative_rgb_bytes: Option<usize>,
        cumulative_rgb_bytes: &mut usize,
    ) -> Result<Option<RasterPageInput>, RasterError> {
        let prefix = read_prefix(&mut self.reader, 4)?;
        if prefix.is_empty() {
            return Ok(None);
        }
        if prefix.len() < 4 {
            return Err(RasterError::StreamParse(format!(
                "Truncated CUPS/PWG Raster page boundary: only {} trailing bytes",
                prefix.len()
            )));
        }

        let mut hdr_buf = [0u8; PWG_HEADER_SIZE];
        if detect_cups_raster_magic(&prefix).is_some() {
            // Multi-page streams may re-emit the 4-byte sync magic before each page header
            read_exact_stream(
                &mut self.reader,
                &mut hdr_buf,
                "Truncated PWG page header after sync magic",
            )?;
        } else {
            hdr_buf[..4].copy_from_slice(&prefix);
            read_exact_stream(
                &mut self.reader,
                &mut hdr_buf[4..],
                "Truncated PWG page header",
            )?;
        }

        if self.pages_emitted >= self.max_pages {
            return Err(RasterError::StreamParse(format!(
                "PWG Raster stream exceeded maximum page limit of {}",
                self.max_pages
            )));
        }

        let header = PwgRasterHeader::from_bytes_with_endian(&hdr_buf, self.little_endian)?;
        let width = header.width_px;
        let height = header.height_px;
        let dpi = header.dpi_x.max(header.dpi_y);

        if width == 0 || height == 0 || header.bits_per_pixel == 0 {
            return Err(RasterError::InvalidDimensions { width, height });
        }
        if width > MAX_PWG_WIDTH_PX || height > MAX_PWG_HEIGHT_PX {
            return Err(RasterError::InvalidDimensions { width, height });
        }
        if header.bits_per_pixel > 64 {
            return Err(RasterError::StreamParse(format!(
                "Unsupported PWG bits_per_pixel: {}",
                header.bits_per_pixel
            )));
        }

        let rgb_line_bytes = (width as usize).checked_mul(3).ok_or_else(|| {
            RasterError::StreamParse("PWG scanline RGB byte count overflow".to_string())
        })?;
        let rgb_total_bytes = rgb_line_bytes.checked_mul(height as usize).ok_or_else(|| {
            RasterError::StreamParse("PWG page RGB buffer size overflow".to_string())
        })?;
        if rgb_total_bytes > MAX_PWG_RGB_BYTES {
            return Err(RasterError::StreamParse(format!(
                "PWG page RGB size {rgb_total_bytes} exceeds maximum allowed {MAX_PWG_RGB_BYTES} bytes"
            )));
        }
        if let Some(budget) = max_cumulative_rgb_bytes {
            let next_cumulative = cumulative_rgb_bytes
                .checked_add(rgb_total_bytes)
                .ok_or_else(|| {
                    RasterError::StreamParse("Cumulative PWG RGB byte count overflow".to_string())
                })?;
            if next_cumulative > budget {
                return Err(RasterError::StreamParse(format!(
                    "Cumulative PWG RGB size {next_cumulative} exceeds maximum allowed {budget} bytes"
                )));
            }
            *cumulative_rgb_bytes = next_cumulative;
        }

        let line_bits = (width as usize)
            .checked_mul(header.bits_per_pixel as usize)
            .ok_or_else(|| {
                RasterError::StreamParse("PWG scanline bit count overflow".to_string())
            })?;
        let min_line_bytes = line_bits.div_ceil(8);
        if min_line_bytes > MAX_PWG_LINE_BYTES || (header.bytes_per_line as usize) > MAX_PWG_LINE_BYTES
        {
            return Err(RasterError::StreamParse(format!(
                "PWG scanline byte length exceeds maximum allowed {MAX_PWG_LINE_BYTES} bytes"
            )));
        }

        let page_rgb = if !self.compressed {
            let row_stride = (header.bytes_per_line as usize).max(min_line_bytes);
            if row_stride.checked_mul(height as usize).is_none() {
                return Err(RasterError::StreamParse(
                    "CUPS raster page size overflow".to_string(),
                ));
            }
            let mut buf = Vec::with_capacity(rgb_total_bytes.min(MAX_PWG_EAGER_PREALLOC_BYTES));
            let mut row_buf = vec![0u8; row_stride];
            for _ in 0..height {
                read_exact_stream(
                    &mut self.reader,
                    &mut row_buf,
                    "Truncated uncompressed CUPS raster page",
                )?;
                convert_scanline_to_rgb(&row_buf, width, &header, self.little_endian, &mut buf)?;
            }
            buf
        } else {
            let mut buf = Vec::with_capacity(rgb_total_bytes.min(MAX_PWG_EAGER_PREALLOC_BYTES));
            let pack_unit = ((header.bits_per_pixel as usize) / 8).max(1);
            let expected_line_bytes = if header.bits_per_pixel < 8 {
                min_line_bytes
            } else {
                (width as usize).checked_mul(pack_unit).ok_or_else(|| {
                    RasterError::StreamParse("PWG compressed scanline size overflow".to_string())
                })?
            };
            if expected_line_bytes > MAX_PWG_LINE_BYTES {
                return Err(RasterError::StreamParse(format!(
                    "PWG compressed scanline {expected_line_bytes} exceeds maximum allowed {MAX_PWG_LINE_BYTES}"
                )));
            }

            let mut lines_decoded = 0u32;
            let mut line_bytes = Vec::with_capacity(expected_line_bytes);
            let mut rgb_line = Vec::with_capacity(rgb_line_bytes);
            let mut pixel_scratch = vec![0u8; pack_unit];

            while lines_decoded < height {
                let Some(repeat_byte) = read_u8_stream(&mut self.reader)? else {
                    return Err(RasterError::StreamParse(format!(
                        "Unexpected EOF before completing PackBits page ({lines_decoded}/{height} lines)"
                    )));
                };
                let repeat_count = repeat_byte as u32;

                line_bytes.clear();
                while line_bytes.len() < expected_line_bytes {
                    let Some(control) = read_u8_stream(&mut self.reader)? else {
                        return Err(RasterError::StreamParse(
                            "Unexpected EOF during PackBits scanline decoding".to_string(),
                        ));
                    };

                    if control <= 127 {
                        let count = (control as usize) + 1;
                        read_exact_stream(
                            &mut self.reader,
                            &mut pixel_scratch,
                            "PackBits buffer overrun on identical run",
                        )?;
                        for _ in 0..count {
                            line_bytes.extend_from_slice(&pixel_scratch);
                        }
                    } else if control >= 129 {
                        let count = 257 - (control as usize);
                        let needed = count.checked_mul(pack_unit).ok_or_else(|| {
                            RasterError::StreamParse("PackBits literal run size overflow".to_string())
                        })?;
                        let start = line_bytes.len();
                        line_bytes.resize(start + needed, 0);
                        read_exact_stream(
                            &mut self.reader,
                            &mut line_bytes[start..start + needed],
                            "PackBits buffer overrun on literal run",
                        )?;
                    }
                }

                if line_bytes.len() != expected_line_bytes {
                    return Err(RasterError::StreamParse(format!(
                        "PackBits scanline decoded {} bytes, expected exact {}",
                        line_bytes.len(),
                        expected_line_bytes
                    )));
                }

                rgb_line.clear();
                convert_scanline_to_rgb(&line_bytes, width, &header, self.little_endian, &mut rgb_line)?;

                let repetitions = (repeat_count + 1).min(height - lines_decoded);
                let needed_total = buf
                    .len()
                    .checked_add((repetitions as usize).saturating_mul(rgb_line.len()))
                    .ok_or_else(|| {
                        RasterError::StreamParse("Decoded PWG page size overflow".to_string())
                    })?;
                if needed_total > MAX_PWG_RGB_BYTES {
                    return Err(RasterError::StreamParse(
                        "Decoded PWG page exceeded maximum byte limit".to_string(),
                    ));
                }
                for _ in 0..repetitions {
                    buf.extend_from_slice(&rgb_line);
                }
                lines_decoded += repetitions;
            }
            buf
        };

        self.pages_emitted += 1;
        Ok(Some(RasterPageInput::new(width, height, dpi, page_rgb)?))
    }
}

/// Parses a PWG / CUPS Raster v2 (`RaS2`/`2SaR`) or v3 (`RaS3`/`3SaR`) stream into pages.
pub fn parse_pwg_raster(stream: &[u8]) -> Result<Vec<RasterPageInput>, RasterError> {
    parse_pwg_raster_with_limits(stream, MAX_PWG_TOTAL_RGB_BYTES, MAX_PWG_PAGES)
}

fn parse_pwg_raster_with_limits(
    stream: &[u8],
    max_total_rgb_bytes: usize,
    max_pages: usize,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let mut reader = PwgRasterReader::with_max_pages(Cursor::new(stream), max_pages)?;
    let mut pages = Vec::new();
    let mut cumulative_rgb_bytes = 0usize;

    while let Some(page) =
        reader.next_page_with_budget(Some(max_total_rgb_bytes), &mut cumulative_rgb_bytes)?
    {
        pages.push(page);
    }

    if pages.is_empty() {
        return Err(RasterError::StreamParse(
            "PWG Raster stream contained no complete pages".to_string(),
        ));
    }

    Ok(pages)
}

/// Decodes standard image file bytes (PNG, JPEG, TIFF, WebP) into a [`RasterPageInput`].
pub fn parse_image_from_memory(
    data: &[u8],
    default_dpi: Option<u32>,
) -> Result<RasterPageInput, RasterError> {
    let img = image::load_from_memory(data)?;
    let rgb = img.to_rgb8();
    let (width, height) = rgb.dimensions();
    let dpi = default_dpi.unwrap_or(300);
    RasterPageInput::new(width, height, dpi, rgb.into_raw())
}

/// Streaming multi-format document source (`RaS2`/`2SaR`/`RaS3`/`3SaR` CUPS/PWG Raster,
/// `%PDF-`, or standard images) yielding decoded RGB pages strictly one at a time.
pub struct DocumentPageSource<'a> {
    inner: DocumentPageSourceInner<'a>,
}

enum DocumentPageSourceInner<'a> {
    Pwg(PwgRasterReader<Box<dyn BufRead + 'a>>),
    Pdf {
        _guard: TempDirGuard,
        png_paths: Vec<PathBuf>,
        next_idx: usize,
        dpi: u32,
    },
    Single(Option<RasterPageInput>),
}

impl DocumentPageSource<'static> {
    /// Wraps a single in-memory [`RasterPageInput`] page as a [`DocumentPageSource`].
    pub fn from_page(page: RasterPageInput) -> Self {
        Self {
            inner: DocumentPageSourceInner::Single(Some(page)),
        }
    }
}

impl<'a> DocumentPageSource<'a> {
    /// Opens a buffered document reader without eagerly decoding or buffering
    /// all multi-page RGB rasters in memory.
    pub fn from_reader<R: BufRead + 'a>(
        mut reader: R,
        default_dpi: Option<u32>,
    ) -> Result<Self, RasterError> {
        let prefix = read_prefix(&mut reader, 64)?;
        if prefix.is_empty() {
            return Err(RasterError::StreamParse(
                "Empty print input stream".to_string(),
            ));
        }

        let dpi = default_dpi.unwrap_or(600).max(75);
        if detect_cups_raster_magic(&prefix).is_some() {
            let chained: Box<dyn BufRead + 'a> =
                Box::new(BufReader::new(Cursor::new(prefix).chain(reader)));
            return Ok(Self {
                inner: DocumentPageSourceInner::Pwg(PwgRasterReader::new(chained)?),
            });
        }

        if prefix.starts_with(b"%PDF") || prefix.windows(5).any(|w| w == b"%PDF-") {
            let work_guard = TempDirGuard::create_private("pixma_pdf")?;
            let pdf_path = work_guard.path().join("input.pdf");
            {
                let mut pdf_file = std::fs::File::create(&pdf_path)?;
                pdf_file.write_all(&prefix)?;
                std::io::copy(&mut reader, &mut pdf_file)?;
                pdf_file.flush()?;
            }
            let png_paths = rasterize_pdf_file_to_png_paths(work_guard.path(), &pdf_path, dpi)?;
            if png_paths.is_empty() {
                return Err(RasterError::StreamParse(
                    "PDF rasterization produced 0 pages".to_string(),
                ));
            }
            if png_paths.len() > MAX_PWG_PAGES {
                return Err(RasterError::StreamParse(format!(
                    "PDF rasterization exceeded maximum page limit of {MAX_PWG_PAGES}"
                )));
            }
            return Ok(Self {
                inner: DocumentPageSourceInner::Pdf {
                    _guard: work_guard,
                    png_paths,
                    next_idx: 0,
                    dpi,
                },
            });
        }

        let mut img_bytes = prefix;
        reader.read_to_end(&mut img_bytes)?;
        let page = parse_image_from_memory(&img_bytes, Some(dpi))?;
        Ok(Self {
            inner: DocumentPageSourceInner::Single(Some(page)),
        })
    }

    /// Returns the next page from the document stream, or `Ok(None)` at EOF.
    pub fn next_page(&mut self) -> Result<Option<RasterPageInput>, RasterError> {
        match &mut self.inner {
            DocumentPageSourceInner::Pwg(reader) => reader.next_page(),
            DocumentPageSourceInner::Pdf {
                png_paths,
                next_idx,
                dpi,
                ..
            } => {
                let Some(path) = png_paths.get(*next_idx) else {
                    return Ok(None);
                };
                let bytes = std::fs::read(path).map_err(|e| {
                    RasterError::StreamParse(format!("Failed to read rasterized PDF page: {e}"))
                })?;
                let _ = std::fs::remove_file(path);
                let page = parse_image_from_memory(&bytes, Some(*dpi))?;
                *next_idx += 1;
                Ok(Some(page))
            }
            DocumentPageSourceInner::Single(opt) => Ok(opt.take()),
        }
    }

    /// Collects all remaining pages into memory while enforcing a bounded cumulative RGB budget
    /// (used only for multi-page impositions like N-Up, Booklet, Reverse Order, and Collated copies).
    pub fn collect_remaining_bounded(
        &mut self,
        max_total_rgb_bytes: usize,
        max_pages: usize,
    ) -> Result<Vec<RasterPageInput>, RasterError> {
        let mut pages = Vec::new();
        let mut cumulative_rgb_bytes = 0usize;
        while let Some(page) = self.next_page()? {
            if pages.len() >= max_pages {
                return Err(RasterError::StreamParse(format!(
                    "Document exceeded maximum page limit of {max_pages}"
                )));
            }
            let page_bytes = page.rgb_pixels.len();
            cumulative_rgb_bytes = cumulative_rgb_bytes
                .checked_add(page_bytes)
                .ok_or_else(|| {
                    RasterError::StreamParse(
                        "Cumulative multi-page RGB byte count overflow".to_string(),
                    )
                })?;
            if cumulative_rgb_bytes > max_total_rgb_bytes {
                return Err(RasterError::StreamParse(format!(
                    "Cumulative multi-page RGB size {cumulative_rgb_bytes} exceeds maximum allowed {max_total_rgb_bytes} bytes"
                )));
            }
            pages.push(page);
        }
        Ok(pages)
    }
}

/// Decodes a document buffer (`RaS2`/`2SaR`/`RaS3`/`3SaR` PWG/CUPS Raster, multi-page `%PDF` document, or
/// PNG/JPEG/TIFF/WebP image) into one or more [`RasterPageInput`] pages.
pub fn parse_document_from_memory(
    data: &[u8],
    default_dpi: Option<u32>,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let dpi = default_dpi.unwrap_or(600).max(75);
    if detect_cups_raster_magic(data).is_some() {
        return parse_pwg_raster(data);
    }
    if data.starts_with(b"%PDF") || data.iter().take(64).copied().collect::<Vec<u8>>().windows(5).any(|w| w == b"%PDF-") {
        return rasterize_pdf_from_memory(data, dpi);
    }
    let page = parse_image_from_memory(data, Some(dpi))?;
    Ok(vec![page])
}

struct TempDirGuard {
    path: std::path::PathBuf,
}

impl TempDirGuard {
    fn create_private(prefix: &str) -> Result<Self, RasterError> {
        use std::sync::atomic::{AtomicU64, Ordering};
        use std::time::{SystemTime, UNIX_EPOCH};

        static SEQ: AtomicU64 = AtomicU64::new(0);
        let base = std::env::temp_dir();
        let pid = std::process::id();

        for _ in 0..64 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let seq = SEQ.fetch_add(1, Ordering::Relaxed);
            let candidate = base.join(format!("{prefix}_{pid}_{nanos}_{seq}"));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                builder.mode(0o700);
            }
            match builder.create(&candidate) {
                Ok(()) => return Ok(Self { path: candidate }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(RasterError::StreamParse(format!(
                        "Failed to create private temp PDF dir: {e}"
                    )))
                }
            }
        }
        Err(RasterError::StreamParse(
            "Exhausted unique names creating temp PDF dir".to_string(),
        ))
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for TempDirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn list_sorted_png_paths(work_dir: &std::path::Path) -> Result<Vec<PathBuf>, RasterError> {
    let mut png_paths: Vec<PathBuf> = std::fs::read_dir(work_dir)
        .map_err(|e| RasterError::StreamParse(format!("Failed to read temp PDF dir: {e}")))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    png_paths.sort();
    Ok(png_paths)
}

fn rasterize_pdf_file_to_png_paths(
    work_dir: &std::path::Path,
    pdf_path: &std::path::Path,
    dpi: u32,
) -> Result<Vec<PathBuf>, RasterError> {
    use std::process::{Command, Stdio};

    let out_prefix = work_dir.join("page");
    let dpi_str = dpi.to_string();
    let max_last_page = (MAX_PWG_PAGES + 1).to_string();
    let pdftoppm_ok = Command::new("pdftoppm")
        .args([
            "-png",
            "-r",
            &dpi_str,
            "-f",
            "1",
            "-l",
            &max_last_page,
            pdf_path.to_str().unwrap_or_default(),
            out_prefix.to_str().unwrap_or_default(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !pdftoppm_ok {
        // Remove any partial PNGs left behind by a failed pdftoppm attempt before invoking gs.
        if let Ok(stale) = list_sorted_png_paths(work_dir) {
            for p in stale {
                let _ = std::fs::remove_file(p);
            }
        }
        let gs_out = format!("{}-%04d.png", out_prefix.display());
        let gs_dpi = format!("-r{dpi}");
        let gs_last_page = format!("-dLastPage={}", MAX_PWG_PAGES + 1);
        let gs_ok = Command::new("gs")
            .args([
                "-q",
                "-dSAFER",
                "-dBATCH",
                "-dNOPAUSE",
                "-sDEVICE=png16m",
                &gs_dpi,
                "-dFirstPage=1",
                &gs_last_page,
                &format!("-sOutputFile={gs_out}"),
                pdf_path.to_str().unwrap_or_default(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !gs_ok {
            return Err(RasterError::StreamParse(
                "Neither pdftoppm nor gs succeeded in rasterizing PDF input".to_string(),
            ));
        }
    }

    list_sorted_png_paths(work_dir)
}

/// Rasterizes a multi-page PDF buffer into [`RasterPageInput`] pages at `dpi` using
/// system `pdftoppm` (or `gs` fallback).
fn rasterize_pdf_from_memory(
    pdf_bytes: &[u8],
    dpi: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let work_guard = TempDirGuard::create_private("pixma_pdf")?;
    let work_dir = work_guard.path();

    let pdf_path = work_dir.join("input.pdf");
    std::fs::write(&pdf_path, pdf_bytes).map_err(|e| {
        RasterError::StreamParse(format!("Failed to write temp PDF file: {e}"))
    })?;

    let png_paths = rasterize_pdf_file_to_png_paths(work_dir, &pdf_path, dpi)?;
    load_rasterized_pdf_pngs(&png_paths, dpi, MAX_PWG_TOTAL_RGB_BYTES, MAX_PWG_PAGES)
}

fn load_rasterized_pdf_pngs(
    png_paths: &[std::path::PathBuf],
    dpi: u32,
    max_total_rgb_bytes: usize,
    max_pages: usize,
) -> Result<Vec<RasterPageInput>, RasterError> {
    if png_paths.len() > max_pages {
        return Err(RasterError::StreamParse(format!(
            "PDF rasterization exceeded maximum page limit of {max_pages}"
        )));
    }

    let mut pages = Vec::with_capacity(png_paths.len());
    let mut cumulative_rgb_bytes: usize = 0;
    for png_path in png_paths {
        let bytes = std::fs::read(png_path).map_err(|e| {
            RasterError::StreamParse(format!("Failed to read rasterized PDF page: {e}"))
        })?;
        let page = parse_image_from_memory(&bytes, Some(dpi))?;
        let page_bytes = (page.width as usize)
            .checked_mul(page.height as usize)
            .and_then(|px| px.checked_mul(3))
            .ok_or_else(|| RasterError::StreamParse("PDF page RGB byte size overflow".to_string()))?;
        cumulative_rgb_bytes = cumulative_rgb_bytes
            .checked_add(page_bytes)
            .ok_or_else(|| RasterError::StreamParse("Cumulative PDF RGB byte count overflow".to_string()))?;
        if cumulative_rgb_bytes > max_total_rgb_bytes {
            return Err(RasterError::StreamParse(format!(
                "Cumulative PDF RGB size {cumulative_rgb_bytes} exceeds maximum allowed {max_total_rgb_bytes} bytes"
            )));
        }
        pages.push(page);
    }

    if pages.is_empty() {
        return Err(RasterError::StreamParse(
            "PDF rasterization produced 0 pages".to_string(),
        ));
    }

    Ok(pages)
}

/// Generates a diagnostic test print and nozzle check calibration page at the given DPI.
pub fn generate_test_print_page(dpi: u32) -> RasterPageInput {
    let dpi = if dpi == 0 { 300 } else { dpi };

    // Standard A4 printable area dimensions in dots
    let (base_w, base_h) = PaperSize::A4.printable_area_dots(dpi, false);

    let max_w = MechanicalBounds::max_carriage_dots_for_dpi(dpi).unwrap_or(base_w);
    let max_h = MechanicalBounds::max_page_height_dots_for_dpi(dpi).unwrap_or(base_h);

    let width = base_w.min(max_w);
    let height = base_h.min(max_h);

    let mut page = RasterPageInput::new_blank(width, height, dpi);

    // 1. Draw top banner (pure Pigment Black on plain paper)
    let banner_h = (height / 25).max(20);
    fill_rect(&mut page, 0, 0, width, banner_h, [0, 0, 0]);

    // 2. Draw border alignment frame
    let margin = (width / 50).max(10);
    draw_rect_outline(
        &mut page,
        margin,
        margin + banner_h,
        width - 2 * margin,
        height - 2 * margin - banner_h,
        [0, 0, 0],
        2,
    );

    // 3. Draw CMYK density step bars
    let colors: [([u8; 3], &'static str); 4] = [
        ([0, 255, 255], "Cyan"),
        ([255, 0, 255], "Magenta"),
        ([255, 255, 0], "Yellow"),
        ([0, 0, 0], "Pigment Black"),
    ];

    let swatch_y_start = banner_h + margin * 2;
    let swatch_h = (height / 35).max(15);
    let steps: [f32; 4] = [1.0, 0.75, 0.5, 0.25];
    let swatch_w = (width - 4 * margin) / 4;

    for (color_idx, (base_rgb, _)) in colors.iter().enumerate() {
        let y = swatch_y_start + (color_idx as u32) * (swatch_h + margin / 2);
        for (step_idx, &density) in steps.iter().enumerate() {
            let x = margin * 2 + (step_idx as u32) * swatch_w;
            let blended = blend_density(*base_rgb, density);
            fill_rect(&mut page, x, y, swatch_w - 4, swatch_h, blended);
        }
    }

    // 4. Draw Nozzle Check Staircase Combs
    let comb_y_start = swatch_y_start + 4 * (swatch_h + margin / 2) + margin;
    let comb_h = (height / 20).max(30);

    for (color_idx, (base_rgb, _)) in colors.iter().enumerate() {
        let cy = comb_y_start + (color_idx as u32) * (comb_h + margin / 2);
        draw_nozzle_comb(
            &mut page,
            margin * 2,
            cy,
            width - 4 * margin,
            comb_h,
            *base_rgb,
        );
    }

    // 5. Center alignment crosshairs
    let cx = width / 2;
    let cy = height / 2;
    let cross_len = (width / 20).max(20);
    draw_line_horizontal(
        &mut page,
        cx.saturating_sub(cross_len),
        cx + cross_len,
        cy,
        [0, 0, 0],
    );
    draw_line_vertical(
        &mut page,
        cx,
        cy.saturating_sub(cross_len),
        cy + cross_len,
        [0, 0, 0],
    );

    // 6. Millimeter-accurate 100.0 mm x 100.0 mm and 50.0 mm x 50.0 mm calibration squares
    //    with 5 mm / 10 mm ruler tick marks in the lower half of the sheet
    let mm_to_dots = |mm: f32| -> u32 { ((mm / 25.4) * (dpi as f32)).round() as u32 };
    let box_100mm = mm_to_dots(100.0);
    let box_50mm = mm_to_dots(50.0);
    let ruler_top = cy + cross_len + mm_to_dots(8.0);

    if box_100mm > 20 && box_100mm + 4 * margin <= width && ruler_top + box_100mm + 2 * margin <= height {
        let rx = cx.saturating_sub(box_100mm / 2);
        let ry = ruler_top;
        let thick = (dpi / 300).max(1);

        // Outer 100.0 mm x 100.0 mm square (pure PGBK [0,0,0])
        draw_rect_outline(&mut page, rx, ry, box_100mm, box_100mm, [0, 0, 0], thick);

        // Inner 50.0 mm x 50.0 mm square
        let in_x = cx.saturating_sub(box_50mm / 2);
        let in_y = ry + (box_100mm - box_50mm) / 2;
        draw_rect_outline(&mut page, in_x, in_y, box_50mm, box_50mm, [0, 0, 0], thick);

        // 5 mm and 10 mm tick marks along top, bottom, left, and right edges of 100 mm box
        let tick_10mm = mm_to_dots(3.0).max(4);
        let tick_5mm = mm_to_dots(1.5).max(2);
        for mm in (5..=95).step_by(5) {
            let offset = mm_to_dots(mm as f32);
            let tick_len = if mm % 10 == 0 { tick_10mm } else { tick_5mm };
            // Top & bottom ticks
            fill_rect(&mut page, rx + offset, ry, thick, tick_len, [0, 0, 0]);
            fill_rect(
                &mut page,
                rx + offset,
                ry + box_100mm.saturating_sub(tick_len),
                thick,
                tick_len,
                [0, 0, 0],
            );
            // Left & right ticks
            fill_rect(&mut page, rx, ry + offset, tick_len, thick, [0, 0, 0]);
            fill_rect(
                &mut page,
                rx + box_100mm.saturating_sub(tick_len),
                ry + offset,
                tick_len,
                thick,
                [0, 0, 0],
            );
        }

        // Pure neutral PGBK gray step ramp + RGB primary swatches inside the 50 mm square
        let pad_in = mm_to_dots(5.0);
        let bar_w = (box_50mm.saturating_sub(2 * pad_in)) / 6;
        let bar_h = mm_to_dots(12.0);
        let gray_levels: [[u8; 3]; 6] = [
            [0, 0, 0],
            [51, 51, 51],
            [102, 102, 102],
            [153, 153, 153],
            [204, 204, 204],
            [230, 230, 230],
        ];
        for (i, &g_rgb) in gray_levels.iter().enumerate() {
            fill_rect(
                &mut page,
                in_x + pad_in + (i as u32) * bar_w,
                in_y + pad_in,
                bar_w.saturating_sub(2),
                bar_h,
                g_rgb,
            );
        }

        let rgb_primaries: [[u8; 3]; 6] = [
            [255, 0, 0],
            [0, 180, 0],
            [0, 0, 255],
            [0, 255, 255],
            [255, 0, 255],
            [255, 255, 0],
        ];
        for (i, &c_rgb) in rgb_primaries.iter().enumerate() {
            fill_rect(
                &mut page,
                in_x + pad_in + (i as u32) * bar_w,
                in_y + box_50mm.saturating_sub(pad_in + bar_h),
                bar_w.saturating_sub(2),
                bar_h,
                c_rgb,
            );
        }
    }

    page
}

fn fill_rect(page: &mut RasterPageInput, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
    let x_end = (x + w).min(page.width);
    let y_end = (y + h).min(page.height);
    for cy in y..y_end {
        for cx in x..x_end {
            page.set_pixel(cx, cy, rgb);
        }
    }
}

fn draw_rect_outline(
    page: &mut RasterPageInput,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    rgb: [u8; 3],
    thickness: u32,
) {
    fill_rect(page, x, y, w, thickness, rgb);
    fill_rect(page, x, y + h.saturating_sub(thickness), w, thickness, rgb);
    fill_rect(page, x, y, thickness, h, rgb);
    fill_rect(page, x + w.saturating_sub(thickness), y, thickness, h, rgb);
}

fn draw_line_horizontal(page: &mut RasterPageInput, x1: u32, x2: u32, y: u32, rgb: [u8; 3]) {
    if y < page.height {
        let start = x1.min(page.width);
        let end = x2.min(page.width);
        for x in start..end {
            page.set_pixel(x, y, rgb);
        }
    }
}

fn draw_line_vertical(page: &mut RasterPageInput, x: u32, y1: u32, y2: u32, rgb: [u8; 3]) {
    if x < page.width {
        let start = y1.min(page.height);
        let end = y2.min(page.height);
        for y in start..end {
            page.set_pixel(x, y, rgb);
        }
    }
}

fn blend_density(base: [u8; 3], density: f32) -> [u8; 3] {
    let d = density.clamp(0.0, 1.0);
    [
        (255.0 - (255.0 - base[0] as f32) * d + 0.5) as u8,
        (255.0 - (255.0 - base[1] as f32) * d + 0.5) as u8,
        (255.0 - (255.0 - base[2] as f32) * d + 0.5) as u8,
    ]
}

fn draw_nozzle_comb(page: &mut RasterPageInput, x: u32, y: u32, w: u32, h: u32, rgb: [u8; 3]) {
    let num_bars = 32u32;
    let step = w / num_bars;
    for i in 0..num_bars {
        let bar_x = x + i * step;
        let bar_h = (h * (i + 1)) / num_bars;
        fill_rect(page, bar_x, y + (h - bar_h), step.max(2) - 1, bar_h, rgb);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::safety::ValidatedRasterLine;
    use pixma_protocol::pwg_raster::encode_pwg_page;

    #[test]
    fn test_raster_page_input_creation() {
        let page = RasterPageInput::new_blank(100, 50, 300);
        assert_eq!(page.width(), 100);
        assert_eq!(page.height(), 50);
        assert_eq!(page.dpi(), 300);
        assert_eq!(page.rgb_pixels().len(), 100 * 50 * 3);
        assert_eq!(page.get_pixel(10, 10), Some([255, 255, 255]));

        let err = RasterPageInput::new(100, 50, 300, vec![0u8; 10]);
        assert!(matches!(err, Err(RasterError::BufferLengthMismatch { .. })));
    }

    #[test]
    fn test_pwg_raster_round_trip_srgb() {
        let width = 64;
        let height = 32;
        let dpi = 600;

        let mut lines = Vec::new();
        for y in 0..height {
            let mut row = Vec::with_capacity(width as usize * 3);
            for x in 0..width {
                row.push((x * 4) as u8);
                row.push((y * 8) as u8);
                row.push(128u8);
            }
            lines.push(ValidatedRasterLine::try_new(row, width, dpi, 3).unwrap());
        }

        let header = PwgRasterHeader::new_srgb(width, height, dpi);
        let encoded_stream = encode_pwg_page(&header, &lines);

        let parsed_pages = parse_pwg_raster(&encoded_stream).expect("PWG parsing must succeed");
        assert_eq!(parsed_pages.len(), 1);

        let parsed = &parsed_pages[0];
        assert_eq!(parsed.width(), width);
        assert_eq!(parsed.height(), height);
        assert_eq!(parsed.dpi(), dpi);

        // Verify sampled pixels
        assert_eq!(parsed.get_pixel(0, 0), Some([0, 0, 128]));
        assert_eq!(parsed.get_pixel(10, 5), Some([40, 40, 128]));
    }

    #[test]
    fn test_pwg_raster_round_trip_sgray() {
        let width = 32;
        let height = 16;
        let dpi = 300;

        let mut lines = Vec::new();
        for y in 0..height {
            let row: Vec<u8> = (0..width).map(|x| (x * 7 + y * 3) as u8).collect();
            lines.push(ValidatedRasterLine::try_new(row, width, dpi, 1).unwrap());
        }

        let header = PwgRasterHeader::new_sgray(width, height, dpi);
        let encoded_stream = encode_pwg_page(&header, &lines);

        let parsed_pages = parse_pwg_raster(&encoded_stream).expect("PWG parsing must succeed");
        assert_eq!(parsed_pages.len(), 1);

        let parsed = &parsed_pages[0];
        assert_eq!(parsed.width(), width);
        assert_eq!(parsed.height(), height);
        // SGray should replicate gray value into R, G, B
        let expected_val = 10 * 7 + 5 * 3;
        assert_eq!(
            parsed.get_pixel(10, 5),
            Some([expected_val, expected_val, expected_val])
        );
    }

    #[test]
    fn test_generate_test_print_page() {
        let page = generate_test_print_page(300);
        assert!(page.width() > 0);
        assert!(page.height() > 0);
        assert_eq!(page.dpi(), 300);
        assert_eq!(
            page.rgb_pixels().len(),
            (page.width() as usize) * (page.height() as usize) * 3
        );
    }

    #[test]
    fn test_image_parsing_png() {
        let mut img = image::RgbImage::new(16, 16);
        for y in 0..16 {
            for x in 0..16 {
                img.put_pixel(x, y, image::Rgb([x as u8 * 16, y as u8 * 16, 128]));
            }
        }
        let mut png_bytes = Vec::new();
        img.write_to(
            &mut std::io::Cursor::new(&mut png_bytes),
            image::ImageFormat::Png,
        )
        .unwrap();

        let page = parse_image_from_memory(&png_bytes, Some(600)).unwrap();
        assert_eq!(page.width(), 16);
        assert_eq!(page.height(), 16);
        assert_eq!(page.dpi(), 600);
        assert_eq!(page.get_pixel(4, 4), Some([64, 64, 128]));
    }

    #[test]
    fn test_pwg_raster_multi_page() {
        let width = 16;
        let height = 8;
        let dpi = 300;

        let lines_p1: Vec<_> = (0..height)
            .map(|_| {
                ValidatedRasterLine::try_new(vec![100u8; width as usize * 3], width, dpi, 3)
                    .unwrap()
            })
            .collect();
        let lines_p2: Vec<_> = (0..height)
            .map(|_| {
                ValidatedRasterLine::try_new(vec![200u8; width as usize * 3], width, dpi, 3)
                    .unwrap()
            })
            .collect();

        let header = PwgRasterHeader::new_srgb(width, height, dpi);
        let mut stream = encode_pwg_page(&header, &lines_p1);
        let p2_stream = encode_pwg_page(&header, &lines_p2);
        stream.extend_from_slice(&p2_stream);

        let parsed = parse_pwg_raster(&stream).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].get_pixel(0, 0), Some([100, 100, 100]));
        assert_eq!(parsed[1].get_pixel(0, 0), Some([200, 200, 200]));
    }

    #[test]
    fn test_cups_raster_v3_little_endian_uncompressed() {
        let width = 8u32;
        let height = 4u32;
        let mut hdr = [0u8; PWG_HEADER_SIZE];
        hdr[276..280].copy_from_slice(&600u32.to_le_bytes());
        hdr[280..284].copy_from_slice(&600u32.to_le_bytes());
        hdr[372..376].copy_from_slice(&width.to_le_bytes());
        hdr[376..380].copy_from_slice(&height.to_le_bytes());
        hdr[384..388].copy_from_slice(&8u32.to_le_bytes());
        hdr[388..392].copy_from_slice(&24u32.to_le_bytes());
        hdr[392..396].copy_from_slice(&(width * 3).to_le_bytes());
        hdr[400..404].copy_from_slice(&1u32.to_le_bytes()); // CUPS_CSPACE_RGB

        let mut stream = Vec::new();
        stream.extend_from_slice(&CUPS_RASTER_REVSYNC_V3);
        // Emit 2 pages sequentially without repeating magic
        for fill in [10u8, 220u8] {
            stream.extend_from_slice(&hdr);
            stream.extend(std::iter::repeat_n(fill, (width * height * 3) as usize));
        }

        let pages = parse_document_from_memory(&stream, None).unwrap();
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].dpi(), 600);
        assert_eq!(pages[0].get_pixel(3, 2), Some([10, 10, 10]));
        assert_eq!(pages[1].get_pixel(3, 2), Some([220, 220, 220]));
    }

    #[test]
    fn test_pwg_raster_invalid_stream() {
        // Bad magic
        let err = parse_pwg_raster(b"NOT_A_PWG_STREAM");
        assert!(matches!(err, Err(RasterError::StreamParse(_))));

        // Too short
        let err = parse_pwg_raster(b"RaS2");
        assert!(matches!(err, Err(RasterError::StreamParse(_))));
    }

    #[test]
    fn test_pwg_raster_rejects_oversized_and_overflow_dimensions() {
        for (w, h, bpp) in [
            (u32::MAX, u32::MAX, 24u32),
            (16_385, 16, 24),
            (16, 32_769, 24),
            (15_000, 15_000, 24), // Exceeds 512 MiB RGB budget
            (100, 100, u32::MAX), // Unsupported/overflowing bpp
        ] {
            let mut hdr = [0u8; PWG_HEADER_SIZE];
            hdr[276..280].copy_from_slice(&600u32.to_be_bytes());
            hdr[280..284].copy_from_slice(&600u32.to_be_bytes());
            hdr[372..376].copy_from_slice(&w.to_be_bytes());
            hdr[376..380].copy_from_slice(&h.to_be_bytes());
            hdr[384..388].copy_from_slice(&8u32.to_be_bytes());
            hdr[388..392].copy_from_slice(&bpp.to_be_bytes());
            hdr[400..404].copy_from_slice(&1u32.to_be_bytes());

            let mut stream = Vec::with_capacity(4 + PWG_HEADER_SIZE);
            stream.extend_from_slice(b"RaS2");
            stream.extend_from_slice(&hdr);

            let res = parse_pwg_raster(&stream);
            assert!(
                res.is_err(),
                "Crafted header ({w}x{h} @ {bpp}bpp) must fail without OOM or panic"
            );
        }
    }

    #[test]
    fn temp_dir_guard_creates_private_dir_and_cleans_up_on_drop() {
        let created_path = {
            let guard = TempDirGuard::create_private("pixma_test_guard").unwrap();
            let p = guard.path().to_path_buf();
            assert!(p.is_dir());
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
                assert_eq!(mode, 0o700);
            }
            p
        };
        assert!(!created_path.exists());
    }

    #[test]
    fn test_pwg_raster_rejects_multipage_limit_and_cumulative_rgb_budget() {
        let mut hdr = [0u8; PWG_HEADER_SIZE];
        hdr[276..280].copy_from_slice(&300u32.to_be_bytes());
        hdr[280..284].copy_from_slice(&300u32.to_be_bytes());
        hdr[372..376].copy_from_slice(&1u32.to_be_bytes());
        hdr[376..380].copy_from_slice(&1u32.to_be_bytes());
        hdr[384..388].copy_from_slice(&8u32.to_be_bytes());
        hdr[388..392].copy_from_slice(&24u32.to_be_bytes());
        hdr[392..396].copy_from_slice(&3u32.to_be_bytes());
        hdr[400..404].copy_from_slice(&1u32.to_be_bytes());

        // 1. 257 tiny 1x1 uncompressed (RaS3) pages must trigger MAX_PWG_PAGES through parse_pwg_raster
        let mut many_pages = Vec::from(&b"RaS3"[..]);
        for _ in 0..=MAX_PWG_PAGES {
            many_pages.extend_from_slice(&hdr);
            many_pages.extend_from_slice(&[10, 20, 30]);
        }
        let page_err = parse_pwg_raster(&many_pages)
            .expect_err("Stream exceeding MAX_PWG_PAGES must be rejected");
        assert!(
            matches!(page_err, RasterError::StreamParse(ref msg) if msg.contains("maximum page limit")),
            "unexpected err: {page_err:?}"
        );

        // 2. Cumulative RGB byte budget across pages must be checked before allocating later pages
        let mut hdr_20x20 = hdr;
        hdr_20x20[372..376].copy_from_slice(&20u32.to_be_bytes());
        hdr_20x20[376..380].copy_from_slice(&20u32.to_be_bytes());
        hdr_20x20[392..396].copy_from_slice(&60u32.to_be_bytes());

        let mut two_pages = Vec::from(&b"RaS3"[..]);
        for fill in [1u8, 2u8] {
            two_pages.extend_from_slice(&hdr_20x20);
            two_pages.extend(std::iter::repeat_n(fill, 20 * 20 * 3));
        }
        // Single page is 1200 bytes; two pages total 2400 bytes -> reject under 2000-byte total cap
        let budget_err = parse_pwg_raster_with_limits(&two_pages, 2000, MAX_PWG_PAGES)
            .expect_err("Cumulative multi-page RGB budget must be rejected");
        assert!(
            matches!(budget_err, RasterError::StreamParse(ref msg) if msg.contains("Cumulative PWG RGB size")),
            "unexpected err: {budget_err:?}"
        );
    }

    #[test]
    fn test_pwg_raster_rejects_packbits_scanline_overrun() {
        let mut hdr = [0u8; PWG_HEADER_SIZE];
        hdr[276..280].copy_from_slice(&600u32.to_be_bytes());
        hdr[280..284].copy_from_slice(&600u32.to_be_bytes());
        hdr[372..376].copy_from_slice(&2u32.to_be_bytes()); // width = 2px -> 6 bytes expected
        hdr[376..380].copy_from_slice(&1u32.to_be_bytes()); // height = 1px
        hdr[384..388].copy_from_slice(&8u32.to_be_bytes());
        hdr[388..392].copy_from_slice(&24u32.to_be_bytes());
        hdr[392..396].copy_from_slice(&6u32.to_be_bytes());
        hdr[400..404].copy_from_slice(&1u32.to_be_bytes());

        let mut stream = Vec::from(&b"RaS2"[..]);
        stream.extend_from_slice(&hdr);
        stream.push(0); // repeat_count = 0 (1 line)
        stream.push(2); // control = 2 -> repeat next pixel 3 times = 9 bytes != 6 expected
        stream.extend_from_slice(&[0x11, 0x22, 0x33]);

        let err = parse_pwg_raster(&stream)
            .expect_err("PackBits line overrun must return StreamParse error");
        assert!(
            matches!(err, RasterError::StreamParse(ref msg) if msg.contains("expected exact 6")),
            "unexpected err: {err:?}"
        );
    }

    #[test]
    fn test_pdf_rasterization_enforces_page_limit_and_cumulative_rgb_budget() {
        let guard = TempDirGuard::create_private("pixma_pdf_budget_test").unwrap();
        let img = image::RgbImage::from_pixel(20, 20, image::Rgb([10, 20, 30]));
        let p1 = guard.path().join("page-1.png");
        let p2 = guard.path().join("page-2.png");
        img.save(&p1).unwrap();
        img.save(&p2).unwrap();

        // 1. Page limit > max_pages rejected
        let page_err = load_rasterized_pdf_pngs(&[p1.clone(), p2.clone()], 300, 1_000_000, 1)
            .expect_err("Exceeding max PDF page count must fail");
        assert!(matches!(page_err, RasterError::StreamParse(_)));

        // 2. Cumulative RGB budget exceeded across pages (20*20*3 = 1200/page -> 2400 total > 2000)
        let rgb_err = load_rasterized_pdf_pngs(&[p1, p2], 300, 2000, MAX_PWG_PAGES)
            .expect_err("Exceeding cumulative PDF RGB byte budget must fail");
        assert!(matches!(rgb_err, RasterError::StreamParse(_)));
    }

    #[test]
    fn test_document_page_source_streams_partial_reads_and_truncated_packbits() {
        struct TinyChunkReader<'a> {
            inner: &'a [u8],
            pos: usize,
            max_chunk: usize,
        }
        impl<'a> Read for TinyChunkReader<'a> {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.pos >= self.inner.len() {
                    return Ok(0);
                }
                let n = buf.len().min(self.max_chunk).min(self.inner.len() - self.pos);
                buf[..n].copy_from_slice(&self.inner[self.pos..self.pos + n]);
                self.pos += n;
                Ok(n)
            }
        }

        // Build a 3-page PackBits stream where page 2 re-emits RaS2 magic
        let mut hdr = [0u8; PWG_HEADER_SIZE];
        hdr[276..280].copy_from_slice(&600u32.to_be_bytes());
        hdr[280..284].copy_from_slice(&600u32.to_be_bytes());
        hdr[372..376].copy_from_slice(&2u32.to_be_bytes());
        hdr[376..380].copy_from_slice(&2u32.to_be_bytes());
        hdr[384..388].copy_from_slice(&8u32.to_be_bytes());
        hdr[388..392].copy_from_slice(&24u32.to_be_bytes());
        hdr[392..396].copy_from_slice(&6u32.to_be_bytes());
        hdr[400..404].copy_from_slice(&1u32.to_be_bytes());

        let mut stream = Vec::from(&b"RaS2"[..]);
        for (i, val) in [10u8, 120u8, 240u8].into_iter().enumerate() {
            if i == 1 {
                stream.extend_from_slice(b"RaS2");
            }
            stream.extend_from_slice(&hdr);
            // repeat=1 (2 rows), control=1 (repeat pixel 2x = 2px)
            stream.push(1);
            stream.push(1);
            stream.extend_from_slice(&[val, val + 1, val + 2]);
        }

        // Simulate slow pipe yielding 3 bytes per read call
        let slow_reader = BufReader::new(TinyChunkReader {
            inner: &stream,
            pos: 0,
            max_chunk: 3,
        });
        let mut source = DocumentPageSource::from_reader(slow_reader, Some(600)).unwrap();
        let mut decoded = Vec::new();
        while let Some(page) = source.next_page().unwrap() {
            decoded.push(page);
        }
        assert_eq!(decoded.len(), 3);
        assert_eq!(decoded[0].get_pixel(1, 1), Some([10, 11, 12]));
        assert_eq!(decoded[1].get_pixel(0, 0), Some([120, 121, 122]));
        assert_eq!(decoded[2].get_pixel(1, 0), Some([240, 241, 242]));

        // Truncated PackBits page (missing 2nd row) must return StreamParse, not partial page
        let mut trunc = Vec::from(&b"RaS2"[..]);
        trunc.extend_from_slice(&hdr);
        trunc.push(0); // only 1 row instead of height=2
        trunc.push(1);
        trunc.extend_from_slice(&[5, 6, 7]);
        let mut trunc_src =
            DocumentPageSource::from_reader(BufReader::new(Cursor::new(&trunc)), Some(600))
                .unwrap();
        let err = trunc_src
            .next_page()
            .expect_err("Truncated PackBits page must return StreamParse");
        assert!(matches!(err, RasterError::StreamParse(_)));
    }
}
