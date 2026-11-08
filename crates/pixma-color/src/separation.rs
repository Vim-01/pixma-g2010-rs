//! CMYK ink separation for Canon PIXMA MegaTank GI-790 ink architecture.
//!
//! Enforces hardware ink constraints:
//! - **Glossy/Photo Media**: Pigment Black (PGBK) cannot adhere to microporous photo coatings
//!   and flakes off. `k_pgbk` is unconditionally forced to 0, synthesizing deep photographic
//!   blacks using composite CMY dye mixing.
//! - **Plain Paper**: Dynamic GCR/UCR extracts crisp, waterproof 100% PGBK for text
//!   ($R=G=B < 16$), and replaces common neutral density in graphics with PGBK to prevent
//!   paper cockling and reduce dry times.

use pixma_core::MediaType;
use rayon::prelude::*;

/// Default Gray Component Replacement (GCR) factor for plain paper printing.
pub const DEFAULT_PLAIN_GCR_FACTOR: f32 = 0.85;

/// Default maximum total ink coverage for plain paper (approx 240%).
pub const DEFAULT_PLAIN_MAX_TOTAL_INK: u16 = 612;

/// Default maximum total ink coverage for glossy photo media (300% CMY).
pub const DEFAULT_GLOSSY_MAX_TOTAL_INK: u16 = 765;

/// Four-channel pixel holding Cyan, Magenta, Yellow, and Pigment Black (GI-790 PGBK).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CmykPixel {
    /// Cyan dye ink (0..=255).
    pub c: u8,
    /// Magenta dye ink (0..=255).
    pub m: u8,
    /// Yellow dye ink (0..=255).
    pub y: u8,
    /// Pigment Black ink (0..=255). Must be 0 on glossy/coated photo media.
    pub k_pgbk: u8,
}

impl CmykPixel {
    /// Creates a new `CmykPixel`.
    #[inline]
    pub const fn new(c: u8, m: u8, y: u8, k_pgbk: u8) -> Self {
        Self { c, m, y, k_pgbk }
    }

    /// Converts to a 4-byte array `[c, m, y, k_pgbk]`.
    #[inline]
    pub const fn to_bytes(self) -> [u8; 4] {
        [self.c, self.m, self.y, self.k_pgbk]
    }

    /// Creates a pixel from a 4-byte array `[c, m, y, k_pgbk]`.
    #[inline]
    pub const fn from_bytes(bytes: [u8; 4]) -> Self {
        Self {
            c: bytes[0],
            m: bytes[1],
            y: bytes[2],
            k_pgbk: bytes[3],
        }
    }

    /// Total ink coverage as a raw sum of channel levels (0..=1020).
    #[inline]
    pub const fn total_ink(self) -> u16 {
        self.c as u16 + self.m as u16 + self.y as u16 + self.k_pgbk as u16
    }
}

/// Ink separation processor implementing the MegaTank GI-790 separation rules.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InkSeparator {
    media_type: MediaType,
    gcr_factor: f32,
    max_total_ink: u16,
}

impl InkSeparator {
    /// Creates an `InkSeparator` configured for the specified media type with hardware defaults.
    pub fn new(media_type: MediaType) -> Self {
        if media_type.requires_composite_cmy_black() {
            Self {
                media_type,
                gcr_factor: 0.0,
                max_total_ink: DEFAULT_GLOSSY_MAX_TOTAL_INK,
            }
        } else {
            Self {
                media_type,
                gcr_factor: DEFAULT_PLAIN_GCR_FACTOR,
                max_total_ink: DEFAULT_PLAIN_MAX_TOTAL_INK,
            }
        }
    }

    /// Sets a custom GCR factor for plain paper (clamped to 0.0..=1.0).
    ///
    /// Note: On glossy media, `k_pgbk` is unconditionally 0 regardless of GCR factor.
    pub fn with_gcr_factor(mut self, factor: f32) -> Self {
        self.gcr_factor = factor.clamp(0.0, 1.0);
        self
    }

    /// Sets a custom maximum total ink coverage threshold (0..=1020).
    pub fn with_max_total_ink(mut self, max_ink: u16) -> Self {
        self.max_total_ink = max_ink.min(1020);
        self
    }

    /// Returns the target media type.
    #[inline]
    pub const fn media_type(&self) -> MediaType {
        self.media_type
    }

    /// Returns the configured GCR factor.
    #[inline]
    pub const fn gcr_factor(&self) -> f32 {
        self.gcr_factor
    }

    /// Returns the maximum allowed total ink coverage limit.
    #[inline]
    pub const fn max_total_ink(&self) -> u16 {
        self.max_total_ink
    }

    /// Separates a single RGB pixel into CMYK (GI-790 dye CMY + pigment K).
    #[inline]
    pub fn separate_pixel(&self, r: u8, g: u8, b: u8) -> CmykPixel {
        if self.media_type.requires_composite_cmy_black() {
            // HARDWARE RULE: Pigment black cannot adhere to microporous photo coatings.
            // Force k_pgbk = 0 unconditionally!
            let c_raw = 255 - r;
            let m_raw = 255 - g;
            let y_raw = 255 - b;

            let total = c_raw as u16 + m_raw as u16 + y_raw as u16;
            if total > self.max_total_ink && total > 0 {
                let scale = self.max_total_ink as f32 / total as f32;
                CmykPixel {
                    c: ((c_raw as f32) * scale + 0.5) as u8,
                    m: ((m_raw as f32) * scale + 0.5) as u8,
                    y: ((y_raw as f32) * scale + 0.5) as u8,
                    k_pgbk: 0,
                }
            } else {
                CmykPixel {
                    c: c_raw,
                    m: m_raw,
                    y: y_raw,
                    k_pgbk: 0,
                }
            }
        } else {
            // PLAIN / UNCOATED MEDIA:
            // Check for pure or near-pure neutral black text (R=G=B < 16)
            let is_near_black = r < 16 && g < 16 && b < 16;
            let is_neutral = r.abs_diff(g) <= 4 && g.abs_diff(b) <= 4 && r.abs_diff(b) <= 4;

            if is_near_black && is_neutral {
                // Pure waterproof pigment black text with 0% CMY dye halo
                return CmykPixel {
                    c: 0,
                    m: 0,
                    y: 0,
                    k_pgbk: 255,
                };
            }

            // Smart Print rule on plain/uncoated media:
            // Any neutral gray pixel (|R-G| <= 4, |G-B| <= 4, |R-B| <= 4) under standard GCR
            // uses 100% Pigment Black (k_pgbk) and 0% CMY dye ink, preventing color ink waste
            // and eliminating chromatic tint in gray text, tables, and halftones.
            if is_neutral && self.gcr_factor >= DEFAULT_PLAIN_GCR_FACTOR {
                let avg_rgb = ((r as u16 + g as u16 + b as u16 + 1) / 3) as u8;
                return CmykPixel {
                    c: 0,
                    m: 0,
                    y: 0,
                    k_pgbk: 255 - avg_rgb,
                };
            }

            // General colors: Dynamic GCR / UCR
            let c_raw = 255 - r;
            let m_raw = 255 - g;
            let y_raw = 255 - b;

            let min_cmy = c_raw.min(m_raw).min(y_raw);
            let k_pgbk = ((min_cmy as f32) * self.gcr_factor + 0.5) as u8;

            let mut c = c_raw.saturating_sub(k_pgbk);
            let mut m = m_raw.saturating_sub(k_pgbk);
            let mut y = y_raw.saturating_sub(k_pgbk);

            // Respect total ink coverage limit
            let total = (c as u16) + (m as u16) + (y as u16) + (k_pgbk as u16);
            if total > self.max_total_ink {
                let dye_sum = (c as u16) + (m as u16) + (y as u16);
                if dye_sum > 0 {
                    let max_dye = self.max_total_ink.saturating_sub(k_pgbk as u16);
                    let scale = (max_dye as f32) / (dye_sum as f32);
                    c = ((c as f32) * scale + 0.5) as u8;
                    m = ((m as f32) * scale + 0.5) as u8;
                    y = ((y as f32) * scale + 0.5) as u8;
                }
            }

            CmykPixel { c, m, y, k_pgbk }
        }
    }

    /// Separates a single RGB scanline sequentially into a destination `CmykPixel` slice.
    ///
    /// # Panics
    /// Panics if `rgb.len() != cmyk.len() * 3`.
    pub fn separate_scanline(&self, rgb: &[u8], cmyk: &mut [CmykPixel]) {
        assert_eq!(
            rgb.len(),
            cmyk.len() * 3,
            "RGB byte buffer length must equal cmyk.len() * 3"
        );
        for (src, dst) in rgb.chunks_exact(3).zip(cmyk.iter_mut()) {
            *dst = self.separate_pixel(src[0], src[1], src[2]);
        }
    }

    /// Separates an RGB scanline and returns a newly allocated `Vec<CmykPixel>`.
    ///
    /// # Panics
    /// Panics if `rgb.len() % 3 != 0`.
    pub fn separate_scanline_to_vec(&self, rgb: &[u8]) -> Vec<CmykPixel> {
        assert_eq!(
            rgb.len() % 3,
            0,
            "RGB buffer length must be a multiple of 3"
        );
        let count = rgb.len() / 3;
        let mut out = Vec::with_capacity(count);
        for src in rgb.chunks_exact(3) {
            out.push(self.separate_pixel(src[0], src[1], src[2]));
        }
        out
    }

    /// Separates a full RGB raster buffer in parallel using Rayon.
    ///
    /// # Panics
    /// Panics if `rgb.len() != cmyk.len() * 3`.
    pub fn separate_buffer(&self, rgb: &[u8], cmyk: &mut [CmykPixel]) {
        assert_eq!(
            rgb.len(),
            cmyk.len() * 3,
            "RGB byte buffer length must equal cmyk.len() * 3"
        );
        rgb.par_chunks_exact(3)
            .zip(cmyk.par_iter_mut())
            .for_each(|(src, dst)| {
                *dst = self.separate_pixel(src[0], src[1], src[2]);
            });
    }

    /// Separates a full RGB buffer in parallel into a newly allocated `Vec<CmykPixel>`.
    ///
    /// # Panics
    /// Panics if `rgb.len() % 3 != 0`.
    pub fn separate_buffer_to_vec(&self, rgb: &[u8]) -> Vec<CmykPixel> {
        assert_eq!(
            rgb.len() % 3,
            0,
            "RGB buffer length must be a multiple of 3"
        );
        let count = rgb.len() / 3;
        let mut out = vec![CmykPixel::default(); count];
        self.separate_buffer(rgb, &mut out);
        out
    }

    /// Separates an RGB scanline into planar channel slices (C, M, Y, and K planes).
    ///
    /// # Panics
    /// Panics if any planar slice length does not equal `rgb.len() / 3`.
    pub fn separate_scanline_planar(
        &self,
        rgb: &[u8],
        c_plane: &mut [u8],
        m_plane: &mut [u8],
        y_plane: &mut [u8],
        k_plane: &mut [u8],
    ) {
        let count = rgb.len() / 3;
        assert_eq!(c_plane.len(), count, "Cyan plane size mismatch");
        assert_eq!(m_plane.len(), count, "Magenta plane size mismatch");
        assert_eq!(y_plane.len(), count, "Yellow plane size mismatch");
        assert_eq!(k_plane.len(), count, "Black plane size mismatch");

        for (i, chunk) in rgb.chunks_exact(3).enumerate() {
            let pixel = self.separate_pixel(chunk[0], chunk[1], chunk[2]);
            c_plane[i] = pixel.c;
            m_plane[i] = pixel.m;
            y_plane[i] = pixel.y;
            k_plane[i] = pixel.k_pgbk;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_glossy_media_hardware_rule_pgbk_zero_on_pure_black() {
        let glossy_medias = [
            MediaType::GlossyPhoto,
            MediaType::PhotoPlusGlossyII,
            MediaType::PhotoProPlatinum,
            MediaType::PhotoProLuster,
            MediaType::PhotoPlusSemiGloss,
        ];

        for media in glossy_medias {
            assert!(
                media.requires_composite_cmy_black(),
                "{media:?} must report requires_composite_cmy_black == true"
            );

            let separator = InkSeparator::new(media);

            // HARDWARE RULE: Pure #000000 black on glossy paper MUST NOT fire pigment black (k_pgbk == 0)
            // and must produce 100% composite CMY dye black (c == 255, m == 255, y == 255).
            let black_px = separator.separate_pixel(0, 0, 0);
            assert_eq!(
                black_px.k_pgbk, 0,
                "Hardware violation: k_pgbk must be 0 for pure black on {media:?}"
            );
            assert_eq!(
                black_px.c, 255,
                "Cyan must be 255 for composite black on {media:?}"
            );
            assert_eq!(
                black_px.m, 255,
                "Magenta must be 255 for composite black on {media:?}"
            );
            assert_eq!(
                black_px.y, 255,
                "Yellow must be 255 for composite black on {media:?}"
            );
        }
    }

    #[test]
    fn test_glossy_media_pgbk_zero_unconditionally_all_pixels() {
        let separator = InkSeparator::new(MediaType::PhotoPlusGlossyII);

        // Test diverse colors, grays, and shadows
        let test_pixels = [
            (0, 0, 0),       // Pure black
            (5, 5, 5),       // Near-black text
            (10, 10, 10),    // Very dark gray
            (64, 64, 64),    // Dark gray
            (128, 128, 128), // Mid gray
            (200, 200, 200), // Light gray
            (255, 255, 255), // Pure white
            (255, 0, 0),     // Red
            (0, 255, 0),     // Green
            (0, 0, 255),     // Blue
            (0, 255, 255),   // Cyan
            (255, 0, 255),   // Magenta
            (255, 255, 0),   // Yellow
            (12, 4, 30),     // Deep purple
            (1, 15, 8),      // Deep forest green
        ];

        for &(r, g, b) in &test_pixels {
            let px = separator.separate_pixel(r, g, b);
            assert_eq!(
                px.k_pgbk, 0,
                "k_pgbk must be 0 for pixel ({r}, {g}, {b}) on glossy media"
            );
        }
    }

    #[test]
    fn test_plain_paper_pure_black_text_pigment() {
        let separator = InkSeparator::new(MediaType::Plain);
        assert!(!separator.media_type().requires_composite_cmy_black());

        // Pure #000000 black text on plain paper must produce 100% k_pgbk and 0% CMY
        let px = separator.separate_pixel(0, 0, 0);
        assert_eq!(
            px.k_pgbk, 255,
            "Plain paper pure black must produce 100% PGBK (255)"
        );
        assert_eq!(px.c, 0, "Plain paper pure black text must have 0% Cyan");
        assert_eq!(px.m, 0, "Plain paper pure black text must have 0% Magenta");
        assert_eq!(px.y, 0, "Plain paper pure black text must have 0% Yellow");
    }

    #[test]
    fn test_plain_paper_near_black_neutral_text() {
        let separator = InkSeparator::new(MediaType::Plain);

        // Near-pure neutral black text (< 16) maps to 100% PGBK and 0% CMY
        for v in [1, 5, 8, 12, 15] {
            let px = separator.separate_pixel(v, v, v);
            assert_eq!(
                px.k_pgbk, 255,
                "Near-black neutral text ({v}, {v}, {v}) must map to 100% PGBK"
            );
            assert_eq!(px.c, 0);
            assert_eq!(px.m, 0);
            assert_eq!(px.y, 0);
        }
    }

    #[test]
    fn test_plain_paper_dark_saturated_color_not_pure_text() {
        let separator = InkSeparator::new(MediaType::Plain);

        // Highly saturated dark color (e.g. dark red 15, 0, 0) is not neutral text
        let px = separator.separate_pixel(15, 0, 0);
        // Should have dye components and not simply pure black text
        assert!(px.m > 0 || px.y > 0);
    }

    #[test]
    fn test_plain_paper_white() {
        let separator = InkSeparator::new(MediaType::Plain);
        let px = separator.separate_pixel(255, 255, 255);
        assert_eq!(px.c, 0);
        assert_eq!(px.m, 0);
        assert_eq!(px.y, 0);
        assert_eq!(px.k_pgbk, 0);
        assert_eq!(px.total_ink(), 0);
    }

    #[test]
    fn test_plain_paper_gcr_midtones() {
        let separator = InkSeparator::new(MediaType::Plain).with_gcr_factor(0.85);

        // Midtone gray (128, 128, 128)
        let px = separator.separate_pixel(128, 128, 128);
        assert!(
            px.k_pgbk > 80,
            "GCR should extract significant PGBK for mid-gray, got {}",
            px.k_pgbk
        );
        assert!(
            px.c < 50 && px.m < 50 && px.y < 50,
            "UCR should reduce CMY dyes significantly, got c={}, m={}, y={}",
            px.c,
            px.m,
            px.y
        );
        assert!(px.total_ink() <= separator.max_total_ink());
    }

    #[test]
    fn test_total_ink_coverage_limit() {
        let limit = 400;
        let separator = InkSeparator::new(MediaType::Plain)
            .with_gcr_factor(0.5)
            .with_max_total_ink(limit);

        // Very dark color that would otherwise exceed ink limits
        let px = separator.separate_pixel(20, 10, 15);
        assert!(
            px.total_ink() <= limit,
            "Total ink {} exceeds limit {}",
            px.total_ink(),
            limit
        );
    }

    #[test]
    fn test_cmyk_pixel_conversions() {
        let px = CmykPixel::new(10, 20, 30, 40);
        assert_eq!(px.to_bytes(), [10, 20, 30, 40]);
        assert_eq!(CmykPixel::from_bytes([10, 20, 30, 40]), px);
        assert_eq!(px.total_ink(), 100);
    }

    #[test]
    fn test_parallel_buffer_matches_scanline() {
        let separator = InkSeparator::new(MediaType::Plain);

        // 3000 pixels (9000 bytes)
        let mut rgb = Vec::with_capacity(9000);
        for i in 0..3000 {
            rgb.push((i * 7 % 256) as u8);
            rgb.push((i * 13 % 256) as u8);
            rgb.push((i * 31 % 256) as u8);
        }

        let mut seq_out = vec![CmykPixel::default(); 3000];
        separator.separate_scanline(&rgb, &mut seq_out);

        let mut par_out = vec![CmykPixel::default(); 3000];
        separator.separate_buffer(&rgb, &mut par_out);

        assert_eq!(
            seq_out, par_out,
            "Parallel and sequential separation must match exactly"
        );
    }

    #[test]
    fn test_separate_scanline_planar() {
        let separator = InkSeparator::new(MediaType::Plain);
        let rgb = [
            0, 0, 0, // Black text
            255, 255, 255, // White
            255, 0, 0, // Red
            128, 128, 128, // Gray
        ];

        let mut c = [0u8; 4];
        let mut m = [0u8; 4];
        let mut y = [0u8; 4];
        let mut k = [0u8; 4];

        separator.separate_scanline_planar(&rgb, &mut c, &mut m, &mut y, &mut k);

        let interleaved = separator.separate_scanline_to_vec(&rgb);
        for i in 0..4 {
            assert_eq!(c[i], interleaved[i].c);
            assert_eq!(m[i], interleaved[i].m);
            assert_eq!(y[i], interleaved[i].y);
            assert_eq!(k[i], interleaved[i].k_pgbk);
        }
    }
}
