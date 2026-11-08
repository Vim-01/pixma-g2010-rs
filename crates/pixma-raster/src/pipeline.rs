//! End-to-end print raster pipeline.
//!
//! Integrates layout transforms, overscan expansion, ICC / tone color pipelines,
//! MegaTank ink separation, and wire protocol compilation into wire-ready
//! IVEC + PWG Raster (`RaS2`) or IVEC + BJRaster3 (`ESC (`) byte streams.

use crate::cups_reader::RasterPageInput;
use crate::errors::RasterError;
use crate::halftone::{BlueNoiseDitherer, DitherAlgorithm, ErrorDiffusionDitherer};
use crate::layout::{apply_borderless_extension, apply_page_layout};
use pixma_color::{rgb_to_sgray_fixed_point, ColorPipeline, InkSeparator};
use pixma_core::print_params::PrintJobConfig;
use pixma_core::safety::{MechanicalBounds, ValidatedRasterLine};
use pixma_protocol::bjraster3::BjRaster3Encoder;
use pixma_protocol::ivec::{
    build_end_job, build_mode_shift, build_send_data_header, build_set_job_configuration,
    build_set_page_configuration, build_set_print_configuration, build_start_job,
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

                    let gray_bytes: Vec<u8> = (0..width as usize)
                        .map(|i| {
                            rgb_to_sgray_fixed_point(
                                rgb_line[i * 3],
                                rgb_line[i * 3 + 1],
                                rgb_line[i * 3 + 2],
                            )
                        })
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

    /// Compiles a complete wire-ready IVEC XML + ModeShift + BJRaster3 (`ESC (`) print job stream.
    pub fn compile_ivec_bjraster3_job(
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

        // 4. SetPageConfiguration
        stream.extend_from_slice(build_set_page_configuration(config, true).as_bytes());

        // 5. ModeShift to binary BJRaster3 mode
        stream.extend_from_slice(build_mode_shift("print", config.job_id, 1).as_bytes());

        // 6. BJRaster3 command prologue & page data
        for page_lines in &rendered_pages {
            let bjraster3_page = BjRaster3Encoder::encode_page(config, page_lines)?;
            stream.extend_from_slice(&bjraster3_page);
        }

        // 7. EndJob
        stream.extend_from_slice(build_end_job("print", config.job_id).as_bytes());

        Ok(stream)
    }
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
        assert!(text.contains("SetJobConfiguration"));
        assert!(text.contains("StartJob"));
        assert!(text.contains("ModeShift"));
        assert!(text.contains("EndJob"));

        // Must contain BJRaster3 ESC ( commands
        assert!(stream.windows(3).any(|w| w == [0x1b, b'(', b'c']));
        assert!(stream.windows(3).any(|w| w == [0x1b, b'(', b't']));
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
