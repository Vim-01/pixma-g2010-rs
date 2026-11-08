//! Ingestion of CUPS / PWG Raster v2 streams, standard images, and synthetic test pages.
//!
//! Provides [`RasterPageInput`], representing an uncompressed 24-bit sRGB raster page
//! with discrete physical dimensions and resolution, along with parsers for standard
//! CUPS raster streams (`"RaS2"` magic) and image formats (PNG, JPEG, TIFF, WebP).

use crate::errors::RasterError;
use pixma_core::print_params::PaperSize;
use pixma_core::safety::MechanicalBounds;
use pixma_protocol::pwg_raster::{PwgRasterHeader, PWG_HEADER_SIZE, PWG_RASTER_MAGIC};

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

/// Parses a PWG / CUPS Raster v2 byte stream (`"RaS2"` magic) into a sequence of pages.
pub fn parse_pwg_raster(mut stream: &[u8]) -> Result<Vec<RasterPageInput>, RasterError> {
    if stream.len() < 4 + PWG_HEADER_SIZE {
        return Err(RasterError::StreamParse(format!(
            "Stream too short for PWG Raster: length is {} bytes",
            stream.len()
        )));
    }

    // Verify stream starts with "RaS2" magic
    if !stream.starts_with(&PWG_RASTER_MAGIC) {
        return Err(RasterError::StreamParse(
            "Missing PWG Raster RaS2 magic prefix".to_string(),
        ));
    }
    stream = &stream[4..];

    let mut pages = Vec::new();

    while !stream.is_empty() {
        // Some multi-page streams re-emit "RaS2" before each page header
        if stream.starts_with(&PWG_RASTER_MAGIC) {
            stream = &stream[4..];
        }

        if stream.len() < PWG_HEADER_SIZE {
            break;
        }

        let header = PwgRasterHeader::from_bytes(&stream[..PWG_HEADER_SIZE])?;
        stream = &stream[PWG_HEADER_SIZE..];

        let width = header.width_px;
        let height = header.height_px;
        let dpi = header.dpi_x.max(header.dpi_y);
        let bpp = (header.bits_per_pixel / 8) as usize;

        if width == 0 || height == 0 || bpp == 0 {
            return Err(RasterError::InvalidDimensions { width, height });
        }

        let mut page_rgb = Vec::with_capacity((width as usize) * (height as usize) * 3);
        let mut lines_decoded = 0u32;

        while lines_decoded < height && !stream.is_empty() {
            // Read line repeat count
            let repeat_count = stream[0] as u32;
            stream = &stream[1..];

            // Decode pixel runs for a single scanline
            let expected_line_bytes = (width as usize) * bpp;
            let mut line_bytes = Vec::with_capacity(expected_line_bytes);

            while line_bytes.len() < expected_line_bytes {
                if stream.is_empty() {
                    return Err(RasterError::StreamParse(
                        "Unexpected EOF during PackBits scanline decoding".to_string(),
                    ));
                }
                let control = stream[0];
                stream = &stream[1..];

                if control <= 127 {
                    let count = (control as usize) + 1;
                    if stream.len() < bpp {
                        return Err(RasterError::StreamParse(
                            "PackBits buffer overrun on identical run".to_string(),
                        ));
                    }
                    let pixel = &stream[..bpp];
                    stream = &stream[bpp..];
                    for _ in 0..count {
                        line_bytes.extend_from_slice(pixel);
                    }
                } else if control >= 129 {
                    let count = 257 - (control as usize);
                    let needed = count * bpp;
                    if stream.len() < needed {
                        return Err(RasterError::StreamParse(
                            "PackBits buffer overrun on literal run".to_string(),
                        ));
                    }
                    line_bytes.extend_from_slice(&stream[..needed]);
                    stream = &stream[needed..];
                }
                // control 128 is a no-op
            }

            // Convert scanline from native format to 24-bit sRGB
            let mut rgb_line = Vec::with_capacity((width as usize) * 3);
            match bpp {
                1 => {
                    // SGray: repeat single luminance channel across R, G, B
                    for &g in &line_bytes[..expected_line_bytes] {
                        rgb_line.extend_from_slice(&[g, g, g]);
                    }
                }
                3 => {
                    // sRGB
                    rgb_line.extend_from_slice(&line_bytes[..expected_line_bytes]);
                }
                4 => {
                    if header.color_space == 6 {
                        // CMYK -> subtractive RGB estimation
                        for chunk in line_bytes[..expected_line_bytes].chunks_exact(4) {
                            let c = chunk[0] as u16;
                            let m = chunk[1] as u16;
                            let y = chunk[2] as u16;
                            let k = chunk[3] as u16;
                            let r = 255u8.saturating_sub((c + k).min(255) as u8);
                            let g = 255u8.saturating_sub((m + k).min(255) as u8);
                            let b = 255u8.saturating_sub((y + k).min(255) as u8);
                            rgb_line.extend_from_slice(&[r, g, b]);
                        }
                    } else {
                        // RGBA -> drop alpha
                        for chunk in line_bytes[..expected_line_bytes].chunks_exact(4) {
                            rgb_line.extend_from_slice(&[chunk[0], chunk[1], chunk[2]]);
                        }
                    }
                }
                other => {
                    return Err(RasterError::StreamParse(format!(
                        "Unsupported bits per pixel: {}",
                        other * 8
                    )));
                }
            }

            let repetitions = (repeat_count + 1).min(height - lines_decoded);
            for _ in 0..repetitions {
                page_rgb.extend_from_slice(&rgb_line);
            }
            lines_decoded += repetitions;
        }

        pages.push(RasterPageInput::new(width, height, dpi, page_rgb)?);
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

/// Decodes a document buffer (`RaS2` PWG/CUPS Raster, multi-page `%PDF` document, or
/// PNG/JPEG/TIFF/WebP image) into one or more [`RasterPageInput`] pages.
pub fn parse_document_from_memory(
    data: &[u8],
    default_dpi: Option<u32>,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let dpi = default_dpi.unwrap_or(600).max(75);
    if data.starts_with(&PWG_RASTER_MAGIC) {
        return parse_pwg_raster(data);
    }
    if data.starts_with(b"%PDF") || data.iter().take(64).copied().collect::<Vec<u8>>().windows(5).any(|w| w == b"%PDF-") {
        return rasterize_pdf_from_memory(data, dpi);
    }
    let page = parse_image_from_memory(data, Some(dpi))?;
    Ok(vec![page])
}

/// Rasterizes a multi-page PDF buffer into [`RasterPageInput`] pages at `dpi` using
/// system `pdftoppm` (or `gs` fallback).
fn rasterize_pdf_from_memory(
    pdf_bytes: &[u8],
    dpi: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let pid = std::process::id();
    let work_dir = std::env::temp_dir().join(format!("pixma_pdf_{pid}_{nanos}"));
    std::fs::create_dir_all(&work_dir)
        .map_err(|e| RasterError::StreamParse(format!("Failed to create temp PDF dir: {e}")))?;

    let pdf_path = work_dir.join("input.pdf");
    let out_prefix = work_dir.join("page");
    if let Err(e) = std::fs::write(&pdf_path, pdf_bytes) {
        let _ = std::fs::remove_dir_all(&work_dir);
        return Err(RasterError::StreamParse(format!(
            "Failed to write temp PDF file: {e}"
        )));
    }

    let dpi_str = dpi.to_string();
    let pdftoppm_ok = Command::new("pdftoppm")
        .args([
            "-png",
            "-r",
            &dpi_str,
            pdf_path.to_str().unwrap_or_default(),
            out_prefix.to_str().unwrap_or_default(),
        ])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    if !pdftoppm_ok {
        let gs_out = format!("{}-%04d.png", out_prefix.display());
        let gs_dpi = format!("-r{dpi}");
        let gs_ok = Command::new("gs")
            .args([
                "-dSAFER",
                "-dBATCH",
                "-dNOPAUSE",
                "-sDEVICE=png16m",
                &gs_dpi,
                &format!("-sOutputFile={gs_out}"),
                pdf_path.to_str().unwrap_or_default(),
            ])
            .status()
            .map(|s| s.success())
            .unwrap_or(false);

        if !gs_ok {
            let _ = std::fs::remove_dir_all(&work_dir);
            return Err(RasterError::StreamParse(
                "Neither pdftoppm nor gs succeeded in rasterizing PDF input".to_string(),
            ));
        }
    }

    let mut png_paths: Vec<std::path::PathBuf> = std::fs::read_dir(&work_dir)
        .map_err(|e| RasterError::StreamParse(format!("Failed to read temp PDF dir: {e}")))?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("png"))
        .collect();
    png_paths.sort();

    let mut pages = Vec::with_capacity(png_paths.len());
    for png_path in &png_paths {
        match std::fs::read(png_path) {
            Ok(bytes) => match parse_image_from_memory(&bytes, Some(dpi)) {
                Ok(page) => pages.push(page),
                Err(e) => {
                    let _ = std::fs::remove_dir_all(&work_dir);
                    return Err(e);
                }
            },
            Err(e) => {
                let _ = std::fs::remove_dir_all(&work_dir);
                return Err(RasterError::StreamParse(format!(
                    "Failed to read rasterized PDF page: {e}"
                )));
            }
        }
    }

    let _ = std::fs::remove_dir_all(&work_dir);

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
    fn test_pwg_raster_invalid_stream() {
        // Bad magic
        let err = parse_pwg_raster(b"NOT_A_PWG_STREAM");
        assert!(matches!(err, Err(RasterError::StreamParse(_))));

        // Too short
        let err = parse_pwg_raster(b"RaS2");
        assert!(matches!(err, Err(RasterError::StreamParse(_))));
    }
}
