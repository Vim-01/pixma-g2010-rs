//! End-to-end print raster pipeline.
//!
//! Integrates layout transforms, overscan expansion, ICC / tone color pipelines,
//! MegaTank ink separation, and wire protocol compilation into wire-ready
//! IVEC + PWG Raster (`RaS2`) or IVEC + BJRaster3 (`ESC (`) byte streams.

use crate::cups_reader::RasterPageInput;
use crate::errors::RasterError;
use crate::halftone::{
    get_blue_noise_matrix_64x64, BlueNoiseDitherer, DitherAlgorithm, ErrorDiffusionDitherer,
};
use crate::layout::{apply_borderless_extension, apply_page_layout};
use pixma_color::{ColorPipeline, InkSeparator};
use pixma_core::print_params::PrintJobConfig;
use pixma_core::safety::{MechanicalBounds, ValidatedRasterLine};
use pixma_protocol::bjraster3::BjRaster3Encoder;
use pixma_protocol::ivec::{
    build_bjraster_mode_shift, build_bjraster_start_job, build_end_job, build_send_data_header,
    build_set_job_configuration, build_set_page_configuration, build_set_print_configuration,
    build_start_job,
};
use pixma_protocol::pwg_raster::{encode_pwg_page, PwgRasterHeader};

/// The primary print raster pipeline orchestrator.
pub struct PrintRasterPipeline;

impl PrintRasterPipeline {
    /// Renders a document page sequence into physical validated scanlines.
    ///
    /// Pipeline stages:
    /// 1. Computes destination printable dot dimensions from `config.paper_size` and `config.quality`.
    /// 2. Clamps dimensions safely to [`MechanicalBounds::max_carriage_dots_for_dpi`].
    /// 3. Applies the page layout imposition ([`apply_page_layout`]).
    /// 4. Applies borderless overscan bleeding ([`apply_borderless_extension`]).
    /// 5. Applies page ordering (reverse order, collation, copies).
    /// 6. Runs [`ColorPipeline`] tone curves and validates ink separation constraints.
    /// 7. Emits validated scanlines clamped to hardware carriage bounds.
    pub fn render_pages_to_validated_lines(
        pages: &[RasterPageInput],
        config: &PrintJobConfig,
    ) -> Result<Vec<Vec<ValidatedRasterLine>>, RasterError> {
        if pages.is_empty() {
            return Err(RasterError::NoPages);
        }

        let dpi = config.quality.base_raster_dpi();
        let max_carriage_dots = MechanicalBounds::max_carriage_dots_for_dpi(dpi)?;
        let max_page_length_dots = MechanicalBounds::max_page_height_dots_for_dpi(dpi)?;

        // Compute destination printable area dots from paper size and borderless mode
        let (printable_w, printable_h) = config
            .paper_size
            .printable_area_dots(dpi, config.borderless.is_borderless());

        let target_w = printable_w.clamp(1, max_carriage_dots);
        let target_h = printable_h.clamp(1, max_page_length_dots);

        // 1. Layout imposition
        let laid_out_pages = apply_page_layout(pages, config.layout, target_w, target_h)?;

        // 2. Borderless overscan extension
        let extended_pages: Vec<RasterPageInput> = laid_out_pages
            .into_iter()
            .map(|p| apply_borderless_extension(&p, config.borderless))
            .collect();

        // 3. Page ordering: reverse order, copies, and collation
        let ordered_pages = apply_page_ordering(
            extended_pages,
            config.reverse_order,
            config.copies,
            config.collate,
        );

        // 4. Color processing & ink separation validation
        let color_pipe = ColorPipeline::new(config.color);
        let separator = InkSeparator::new(config.media_type);

        let mut rendered_pages = Vec::with_capacity(ordered_pages.len());

        for page in &ordered_pages {
            let width = page.width;
            let height = page.height;
            let mut page_lines = Vec::with_capacity(height as usize);

            if config.color.grayscale {
                // Monochrome path: 8-bit SGray validated raster lines
                for y in 0..height {
                    let start = (y as usize) * (width as usize) * 3;
                    let end = start + (width as usize) * 3;
                    let mut rgb_line = page.rgb_pixels[start..end].to_vec();

                    color_pipe.transform_scanline(&mut rgb_line);

                    // `color_pipe.transform_scanline` in grayscale mode already converts
                    // each pixel to `(gray, gray, gray)` via `rgb_to_sgray_fixed_point` + LUT.
                    let gray_bytes: Vec<u8> = (0..width as usize)
                        .map(|i| rgb_line[i * 3])
                        .collect();

                    let validated_line = ValidatedRasterLine::try_new(gray_bytes, width, dpi, 1)?;
                    page_lines.push(validated_line);
                }
            } else {
                // Color path: 24-bit sRGB validated raster lines
                for y in 0..height {
                    let start = (y as usize) * (width as usize) * 3;
                    let end = start + (width as usize) * 3;
                    let mut rgb_line = page.rgb_pixels[start..end].to_vec();

                    color_pipe.transform_scanline(&mut rgb_line);

                    // HARDWARE INTERLOCK VERIFICATION:
                    // On glossy/coated media, Pigment Black (PGBK) cannot adhere to silica coating
                    // and must be strictly 0. Verify that InkSeparator separates with k_pgbk == 0.
                    if config.media_type.requires_composite_cmy_black() {
                        for i in 0..width as usize {
                            let cmyk = separator.separate_pixel(
                                rgb_line[i * 3],
                                rgb_line[i * 3 + 1],
                                rgb_line[i * 3 + 2],
                            );
                            debug_assert_eq!(
                                cmyk.k_pgbk, 0,
                                "Pigment black must be strictly 0 on glossy media"
                            );
                        }
                    }

                    let validated_line = ValidatedRasterLine::try_new(rgb_line, width, dpi, 3)?;
                    page_lines.push(validated_line);
                }
            }

            rendered_pages.push(page_lines);
        }

        Ok(rendered_pages)
    }

    /// Renders pages and dithers into 1-byte planar halftoned CMYK bands.
    pub fn render_pages_to_cmyk_halftoned_lines(
        pages: &[RasterPageInput],
        config: &PrintJobConfig,
        algorithm: DitherAlgorithm,
    ) -> Result<Vec<Vec<ValidatedRasterLine>>, RasterError> {
        let dpi = config.quality.base_raster_dpi();
        let rendered_pages = Self::render_pages_to_validated_lines(pages, config)?;
        let separator = InkSeparator::new(config.media_type);

        let mut halftoned_pages = Vec::with_capacity(rendered_pages.len());

        for page_lines in rendered_pages {
            let width = page_lines[0].dot_width() as usize;
            let height = page_lines.len();

            // Extract CMYK pixels
            let mut cmyk_pixels = Vec::with_capacity(width * height);
            for line in &page_lines {
                let raw = line.as_bytes();
                if line.bytes_per_pixel() == 1 {
                    for &g in raw {
                        let cmyk = separator.separate_pixel(g, g, g);
                        cmyk_pixels.push(cmyk);
                    }
                } else {
                    for chunk in raw.chunks_exact(3) {
                        let cmyk = separator.separate_pixel(chunk[0], chunk[1], chunk[2]);
                        cmyk_pixels.push(cmyk);
                    }
                }
            }

            // Dither planes
            let (c_dots, m_dots, y_dots, k_dots) = match algorithm {
                DitherAlgorithm::BlueNoise => {
                    let ditherer = BlueNoiseDitherer::new();
                    ditherer.dither_cmyk_2bit(&cmyk_pixels, width, height)
                }
                DitherAlgorithm::Sierra3 => {
                    let ditherer = ErrorDiffusionDitherer::new_sierra3();
                    let c_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.c).collect();
                    let m_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.m).collect();
                    let y_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.y).collect();
                    let k_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.k_pgbk).collect();
                    (
                        ditherer.dither_plane_2bit(&c_in, width, height),
                        ditherer.dither_plane_2bit(&m_in, width, height),
                        ditherer.dither_plane_2bit(&y_in, width, height),
                        ditherer.dither_plane_2bit(&k_in, width, height),
                    )
                }
                DitherAlgorithm::JarvisJudiceNinke => {
                    let ditherer = ErrorDiffusionDitherer::new_jjn();
                    let c_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.c).collect();
                    let m_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.m).collect();
                    let y_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.y).collect();
                    let k_in: Vec<u8> = cmyk_pixels.iter().map(|p| p.k_pgbk).collect();
                    (
                        ditherer.dither_plane_2bit(&c_in, width, height),
                        ditherer.dither_plane_2bit(&m_in, width, height),
                        ditherer.dither_plane_2bit(&y_in, width, height),
                        ditherer.dither_plane_2bit(&k_in, width, height),
                    )
                }
            };

            // Interleave into 4-byte CMYK dot lines
            let mut validated_lines = Vec::with_capacity(height);
            for y in 0..height {
                let mut line_bytes = Vec::with_capacity(width * 4);
                let row_start = y * width;
                for x in 0..width {
                    let idx = row_start + x;
                    line_bytes.push(c_dots[idx]);
                    line_bytes.push(m_dots[idx]);
                    line_bytes.push(y_dots[idx]);
                    line_bytes.push(k_dots[idx]);
                }
                validated_lines.push(ValidatedRasterLine::try_new(
                    line_bytes,
                    width as u32,
                    dpi,
                    4,
                )?);
            }

            halftoned_pages.push(validated_lines);
        }

        Ok(halftoned_pages)
    }

    /// Compiles a complete wire-ready IVEC XML + BJL + PWG Raster (`RaS2`) print job stream.
    pub fn compile_ivec_pwg_job(
        pages: &[RasterPageInput],
        config: &PrintJobConfig,
        timestamp: &str,
    ) -> Result<Vec<u8>, RasterError> {
        let rendered_pages = Self::render_pages_to_validated_lines(pages, config)?;
        let mut stream = Vec::new();

        // 1. StartJob (must precede all job configuration in Canon IVEC protocol)
        stream.extend_from_slice(build_start_job("print", config.job_id).as_bytes());

        // 2. SetJobConfiguration
        stream.extend_from_slice(
            build_set_job_configuration(config.job_id, Some(timestamp)).as_bytes(),
        );

        // 3. SetConfiguration (papersize, papertype, borderless, colormode, duplex)
        stream.extend_from_slice(build_set_print_configuration(config).as_bytes());

        // 4. Per-page configurations and PWG raster payloads
        for (idx, page_lines) in rendered_pages.iter().enumerate() {
            let page_no = (idx + 1) as u32;
            let is_last_page = idx + 1 == rendered_pages.len();

            // SetPageConfiguration
            stream.extend_from_slice(build_set_page_configuration(config, is_last_page).as_bytes());

            // PWG header & encoded page
            let width = page_lines[0].dot_width();
            let height = page_lines.len() as u32;
            let dpi = page_lines[0].dpi();

            let header = if config.color.grayscale {
                PwgRasterHeader::new_sgray(width, height, dpi)
            } else {
                PwgRasterHeader::new_srgb(width, height, dpi)
            };

            let pwg_page_data = encode_pwg_page(&header, page_lines);

            // SendData header + PWG page payload
            stream.extend_from_slice(
                build_send_data_header(config.job_id, page_no, "PWGRaster", pwg_page_data.len())
                    .as_bytes(),
            );
            stream.extend_from_slice(&pwg_page_data);
        }

        // 5. EndJob
        stream.extend_from_slice(build_end_job("print", config.job_id).as_bytes());

        Ok(stream)
    }

    /// Compiles a complete wire-ready IVEC XML + ModeShift + BJR520 (`BJRaster3` `ESC (`) print job stream
    /// verified against physical Canon PIXMA G2010 (`CID:CA_BJR520_IJP`) hardware.
    pub fn compile_ivec_bjraster3_job(
        pages: &[RasterPageInput],
        config: &PrintJobConfig,
        _timestamp: &str,
    ) -> Result<Vec<u8>, RasterError> {
        let rendered_pages = Self::render_pages_to_validated_lines(pages, config)?;
        let separator = InkSeparator::new(config.media_type);
        let mut stream = Vec::new();

        // 1. StartJob (<ivec:bidi>0</ivec:bidi>)
        stream.extend_from_slice(build_bjraster_start_job(config.job_id).as_bytes());

        // 2. VendorCmd ModeShift (ijmode=1 -> binary BJRaster3 stream)
        stream.extend_from_slice(build_bjraster_mode_shift(config.job_id).as_bytes());

        // Determine hardware 600 DPI printable grid geometry
        let (raw_w_600, raw_h_600) = config.paper_size.printable_area_dots(600, false);
        let (printable_w_600, printable_h_600) = match config.paper_size {
            pixma_core::print_params::PaperSize::A4 => (4791u32, 6816u32),
            pixma_core::print_params::PaperSize::Letter => (4933u32, 6400u32),
            pixma_core::print_params::PaperSize::Photo4x6 => (2233u32, 3400u32),
            _ => (raw_w_600.min(4791), (raw_h_600 / 8) * 8),
        };
        let total_bands = (printable_h_600 as usize / 8).max(1);
        let dot_w = printable_w_600 as usize;
        let bytes_1bpp = dot_w.div_ceil(8);
        let bytes_2bpp = dot_w.div_ceil(4);
        let blue_noise = get_blue_noise_matrix_64x64();

        // 3. Per-page BJR520 prologue, 8-line ESC ( F bands, and epilogue
        for (page_idx, page_lines) in rendered_pages.iter().enumerate() {
            let page_no = (page_idx + 1) as u8;
            let is_last_page = page_idx + 1 == rendered_pages.len();

            // Smart Print per-page auto-monochrome detection:
            // On plain/uncoated media, if every pixel on this page is neutral (black/gray/white),
            // switch the hardware page prologue to Monochrome (`ESC ( c = 0x31`, `ESC ( L = "K"`)
            // so only the Pigment Black (`BH-4` / `PGBK`) printhead is engaged for this page.
            let page_grayscale = config.color.grayscale
                || (!config.media_type.requires_composite_cmy_black()
                    && is_page_neutral_monochrome(page_lines));

            let mut page_config = config.clone();
            page_config.color.grayscale = page_grayscale;

            stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_prologue(
                &page_config,
                page_no,
                printable_w_600,
                printable_h_600,
            ));

            let scale = if !page_lines.is_empty() && page_lines[0].dpi() == 300 {
                2usize
            } else {
                1usize
            };

            let mut c_band = vec![Vec::new(); 8];
            let mut m_band = vec![Vec::new(); 8];
            let mut y_band = vec![Vec::new(); 8];
            let mut k_band = vec![Vec::new(); 8];

            for band_idx in 0..total_bands {
                for row in 0..8 {
                    c_band[row].clear();
                    m_band[row].clear();
                    y_band[row].clear();
                    k_band[row].clear();

                    let y_600 = band_idx * 8 + row;
                    let src_y = y_600 / scale;
                    let Some(line) = page_lines.get(src_y) else {
                        continue;
                    };
                    let raw = line.as_bytes();
                    // Fast path: completely white scanline needs 0 ink droplets
                    if raw.iter().all(|&b| b == 0xFF) {
                        continue;
                    }

                    let bn_row_start = (y_600 & 63) * 64;
                    let bn_row = &blue_noise[bn_row_start..bn_row_start + 64];
                    let src_w = line.dot_width() as usize;

                    if line.bytes_per_pixel() == 1 || page_grayscale {
                        k_band[row].resize(bytes_1bpp, 0);
                        let bpp = line.bytes_per_pixel() as usize;
                        for px in 0..dot_w {
                            let sx = (px / scale).min(src_w.saturating_sub(1));
                            let g = if bpp == 3 {
                                let base = sx * 3;
                                ((raw[base] as u16 + raw[base + 1] as u16 + raw[base + 2] as u16 + 1)
                                    / 3) as u8
                            } else {
                                raw[sx * bpp]
                            };
                            if g < 255 {
                                let k = if g < 16 { 255 } else { 255 - g };
                                let thr = bn_row[px & 63];
                                if k > thr {
                                    k_band[row][px >> 3] |= 1 << (7 - (px & 7));
                                }
                            }
                        }
                    } else {
                        c_band[row].resize(bytes_2bpp, 0);
                        m_band[row].resize(bytes_2bpp, 0);
                        y_band[row].resize(bytes_1bpp, 0);
                        k_band[row].resize(bytes_1bpp, 0);

                        for px in 0..dot_w {
                            let sx = (px / scale).min(src_w.saturating_sub(1));
                            let base = sx * 3;
                            let r = raw[base];
                            let g = raw[base + 1];
                            let b = raw[base + 2];
                            if (r & g & b) == 0xFF {
                                continue;
                            }

                            let cmyk = separator.separate_pixel(r, g, b);
                            let thr = bn_row[px & 63];
                            let thr_lo = thr >> 1;
                            let thr_hi = 128 + thr_lo;
                            let shift_2bpp = (3 - (px & 3)) * 2;
                            let bit_1bpp = 1u8 << (7 - (px & 7));

                            if cmyk.c > thr_lo {
                                let lvl: u8 = if cmyk.c > thr_hi { 2 } else { 1 };
                                c_band[row][px >> 2] |= lvl << shift_2bpp;
                            }
                            if cmyk.m > thr_lo {
                                let lvl: u8 = if cmyk.m > thr_hi { 2 } else { 1 };
                                m_band[row][px >> 2] |= lvl << shift_2bpp;
                            }
                            if cmyk.y > thr {
                                y_band[row][px >> 3] |= bit_1bpp;
                            }
                            if cmyk.k_pgbk > thr {
                                k_band[row][px >> 3] |= bit_1bpp;
                            }
                        }
                    }
                }

                if page_grayscale {
                    let k_refs: [&[u8]; 8] = std::array::from_fn(|r| k_band[r].as_slice());
                    stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_band_esc_f(&k_refs));
                } else {
                    let c_refs: [&[u8]; 8] = std::array::from_fn(|r| c_band[r].as_slice());
                    let m_refs: [&[u8]; 8] = std::array::from_fn(|r| m_band[r].as_slice());
                    let y_refs: [&[u8]; 8] = std::array::from_fn(|r| y_band[r].as_slice());
                    let k_refs: [&[u8]; 8] = std::array::from_fn(|r| k_band[r].as_slice());
                    stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_band_esc_f(&c_refs));
                    stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_band_esc_f(&m_refs));
                    stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_band_esc_f(&y_refs));
                    stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_band_esc_f(&k_refs));
                }
            }

            stream.extend_from_slice(&BjRaster3Encoder::encode_bjr520_epilogue(is_last_page));
        }

        // 4. EndJob
        stream.extend_from_slice(build_end_job("print", config.job_id).as_bytes());

        Ok(stream)
    }
}

fn is_page_neutral_monochrome(page_lines: &[ValidatedRasterLine]) -> bool {
    for line in page_lines {
        if line.bytes_per_pixel() == 1 {
            continue;
        }
        for px in line.as_bytes().chunks_exact(3) {
            let r = px[0];
            let g = px[1];
            let b = px[2];
            if r.abs_diff(g) > 4 || g.abs_diff(b) > 4 || r.abs_diff(b) > 4 {
                return false;
            }
        }
    }
    true
}

fn apply_page_ordering(
    mut pages: Vec<RasterPageInput>,
    reverse_order: bool,
    copies: u16,
    collate: bool,
) -> Vec<RasterPageInput> {
    if reverse_order {
        pages.reverse();
    }

    let copies = copies.max(1) as usize;
    if copies == 1 {
        return pages;
    }

    if collate {
        // Collated: Page 1, 2, 3, then Page 1, 2, 3
        let mut out = Vec::with_capacity(pages.len() * copies);
        for _ in 0..copies {
            out.extend(pages.clone());
        }
        out
    } else {
        // Uncollated: Page 1, 1, Page 2, 2, Page 3, 3
        let mut out = Vec::with_capacity(pages.len() * copies);
        for page in pages {
            for _ in 0..copies {
                out.push(page.clone());
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::print_params::{
        BorderlessExtension, MediaType, PageLayout, PaperSize, PrintJobConfig, PrintQuality,
    };

    fn make_test_page(w: u32, h: u32) -> RasterPageInput {
        RasterPageInput::new_blank(w, h, 600)
    }

    #[test]
    fn test_render_pages_to_validated_lines_safety_bounds() {
        let pages = vec![make_test_page(200, 200)];
        let config = PrintJobConfig {
            paper_size: PaperSize::A4,
            quality: PrintQuality::Level3Standard,
            ..Default::default()
        };

        let result = PrintRasterPipeline::render_pages_to_validated_lines(&pages, &config).unwrap();
        assert_eq!(result.len(), 1);

        let lines = &result[0];
        assert!(!lines.is_empty());
        assert_eq!(lines[0].dpi(), 600);
        // Carriage dots at 600 DPI must not exceed 5100
        assert!(lines[0].dot_width() <= MechanicalBounds::MAX_CARRIAGE_DOTS_600DPI);
    }

    #[test]
    fn test_compile_ivec_pwg_job() {
        let pages = vec![make_test_page(100, 100)];
        let config = PrintJobConfig {
            job_id: 42,
            paper_size: PaperSize::Photo4x6,
            quality: PrintQuality::Level3Standard,
            borderless: BorderlessExtension::Extension1Low,
            ..Default::default()
        };

        let stream =
            PrintRasterPipeline::compile_ivec_pwg_job(&pages, &config, "20261106175000").unwrap();

        // Stream must contain IVEC envelope and PWG RaS2 magic
        let text = String::from_utf8_lossy(&stream);
        assert!(text.contains("StartJob"));
        assert!(text.contains("SetJobConfiguration"));
        assert!(text.contains("SetConfiguration"));
        assert!(text.contains("SetPageConfiguration"));
        assert!(text.contains("EndJob"));
        assert!(stream.windows(4).any(|w| w == b"RaS2"));
    }

    #[test]
    fn test_compile_ivec_bjraster3_job() {
        let pages = vec![make_test_page(100, 100)];
        let config = PrintJobConfig {
            job_id: 10,
            paper_size: PaperSize::Photo4x6,
            quality: PrintQuality::Level3Standard,
            layout: PageLayout::Normal,
            ..Default::default()
        };

        let stream =
            PrintRasterPipeline::compile_ivec_bjraster3_job(&pages, &config, "20261106175000")
                .unwrap();

        let text = String::from_utf8_lossy(&stream);
        assert!(text.contains("StartJob"));
        assert!(text.contains("ModeShift"));
        assert!(text.contains("EndJob"));

        // Must contain BJR520 ESC [ K and ESC ( commands
        assert!(stream.windows(3).any(|w| w == [0x1b, b'[', b'K']));
        assert!(stream.windows(3).any(|w| w == [0x1b, b'(', b'c']));
        assert!(stream.windows(3).any(|w| w == [0x1b, b'(', b't']));
        assert!(stream.windows(3).any(|w| w == [0x1b, b'(', b'F']));
    }

    #[test]
    fn test_smart_print_multipage_auto_monochrome_and_color() {
        // Page 1: only black and neutral gray pixels -> must auto-select ESC ( L = "K"
        let mut p1 = make_test_page(32, 32);
        p1.set_pixel(5, 5, [0, 0, 0]);
        p1.set_pixel(10, 10, [128, 128, 128]);

        // Page 2: contains colored pixels -> must select ESC ( L = "CMYK"
        let mut p2 = make_test_page(32, 32);
        p2.set_pixel(5, 5, [0, 0, 0]);
        p2.set_pixel(10, 10, [255, 0, 0]);

        let config = PrintJobConfig {
            job_id: 77,
            paper_size: PaperSize::Photo4x6,
            media_type: MediaType::Plain,
            quality: PrintQuality::Level3Standard,
            ..Default::default()
        };

        let stream =
            PrintRasterPipeline::compile_ivec_bjraster3_job(&[p1, p2], &config, "20261108200000")
                .unwrap();

        // Both monochrome ESC ( L "K" (page 1) and color ESC ( L "CMYK" (page 2) must appear
        assert!(stream
            .windows(6)
            .any(|w| w == [0x1b, b'(', b'L', 0x01, 0x00, b'K']));
        assert!(stream
            .windows(9)
            .any(|w| w == [0x1b, b'(', b'L', 0x04, 0x00, b'C', b'M', b'Y', b'K']));
    }

    #[test]
    fn test_glossy_media_pigment_black_safety_validation() {
        let mut page = make_test_page(50, 50);
        // Paint pitch black (0, 0, 0)
        for y in 0..50 {
            for x in 0..50 {
                page.set_pixel(x, y, [0, 0, 0]);
            }
        }

        let config = PrintJobConfig {
            media_type: MediaType::PhotoPlusGlossyII, // Coated photo paper
            paper_size: PaperSize::Photo4x6,
            quality: PrintQuality::Level2High,
            ..Default::default()
        };

        // Must succeed and enforce k_pgbk == 0 on glossy media
        let lines = PrintRasterPipeline::render_pages_to_validated_lines(&[page], &config).unwrap();
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_grayscale_rendering_mode() {
        let page = make_test_page(40, 40);
        let mut config = PrintJobConfig::default();
        config.color.grayscale = true;

        let rendered =
            PrintRasterPipeline::render_pages_to_validated_lines(&[page], &config).unwrap();
        assert_eq!(rendered.len(), 1);
        let lines = &rendered[0];
        assert_eq!(lines[0].bytes_per_pixel(), 1); // 1 byte per pixel for SGray
    }

    #[test]
    fn test_copies_and_collation_ordering() {
        let p1 = RasterPageInput::new_blank(20, 20, 300);
        let mut p2 = RasterPageInput::new_blank(20, 20, 300);
        p2.set_pixel(0, 0, [0, 0, 0]);

        // Collated 2 copies: [p1, p2, p1, p2]
        let config_collated = PrintJobConfig {
            copies: 2,
            collate: true,
            ..Default::default()
        };
        let res_collated = PrintRasterPipeline::render_pages_to_validated_lines(
            &[p1.clone(), p2.clone()],
            &config_collated,
        )
        .unwrap();
        assert_eq!(res_collated.len(), 4);

        // Uncollated 2 copies: [p1, p1, p2, p2]
        let config_uncollated = PrintJobConfig {
            copies: 2,
            collate: false,
            ..Default::default()
        };
        let res_uncollated =
            PrintRasterPipeline::render_pages_to_validated_lines(&[p1, p2], &config_uncollated)
                .unwrap();
        assert_eq!(res_uncollated.len(), 4);
    }

    #[test]
    fn test_cmyk_halftoned_lines_pipeline() {
        let page = make_test_page(32, 16);
        let config = PrintJobConfig::default();

        let halftoned = PrintRasterPipeline::render_pages_to_cmyk_halftoned_lines(
            &[page],
            &config,
            DitherAlgorithm::BlueNoise,
        )
        .unwrap();
        assert_eq!(halftoned.len(), 1);
        let lines = &halftoned[0];
        assert_eq!(lines[0].bytes_per_pixel(), 4); // CMYK 4 planes
    }
}
