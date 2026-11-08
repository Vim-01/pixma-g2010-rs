//! Color pipeline and tone adjustment lookup tables.
//!
//! Provides high-speed sRGB/ICC transformation and hardware-matched tone adjustments
//! (brightness, contrast, intensity, gamma, and Cyan/Magenta/Yellow channel balance)
//! using precomputed 256-entry lookup tables for $O(N)$ per-pixel execution.

use crate::errors::ColorError;
use pixma_core::ColorAdjustment;
use qcms::{DataType, Intent, Profile, Transform};
use rayon::prelude::*;

/// Exact fixed-point luminance conversion: `((1742 * R + 5859 * G + 591 * B + 4096) >> 13)`.
///
/// Uses the ITU-R BT.709 fixed-point luminance coefficients (0.2126 R + 0.7152 G + 0.0722 B)
/// scaled to 13-bit integer precision (`1 << 13 = 8192`):
/// - `1742` = `round(0.2126 * 8192)`
/// - `5859` = `round(0.7152 * 8192)`
/// - `591`  = `round(0.0722 * 8192)`
///
/// The sum of weights is exactly `8192`, so shifting right by 13 preserves neutral gray
/// (`rgb_to_sgray_fixed_point(v, v, v) == v` for all `v in 0..=255`).
#[inline]
pub fn rgb_to_sgray_fixed_point(r: u8, g: u8, b: u8) -> u8 {
    let raw = (1742 * u32::from(r) + 5859 * u32::from(g) + 591 * u32::from(b) + 4096) >> 13;
    raw.min(255) as u8
}

/// Computes a single entry for a channel lookup table based on user adjustments.
fn compute_channel_lut_entry(
    val: u8,
    brightness: f32,
    contrast: f32,
    intensity: f32,
    gamma: f32,
    balance: f32,
) -> u8 {
    let x = val as f32 / 255.0;

    // 1. Gamma correction relative to sRGB target (default 2.2)
    let x_gamma = if (gamma - 2.2).abs() > 0.001 {
        x.powf(gamma / 2.2)
    } else {
        x
    };

    // 2. Contrast adjustment centered around midtone (0.5)
    let contrast_factor = if contrast >= 0.0 {
        1.0 + (contrast / 50.0)
    } else {
        1.0 + (contrast / 50.0) * 0.8
    };
    let x_contrast = (x_gamma - 0.5) * contrast_factor + 0.5;

    // 3. Brightness shift (-50..=+50 maps to -0.5..=+0.5)
    let x_bright = x_contrast + (brightness / 100.0);

    // 4. Overall ink intensity adjustment (-50..=+50)
    // Subtractive ink density D = 1.0 - x. Higher intensity deposits more ink (darker RGB),
    // while leaving pure white paper (D = 0) unaffected.
    let ink_density = (1.0 - x_bright).max(0.0);
    let ink_adjusted = ink_density * (1.0 + intensity / 100.0);
    let x_intensity = 1.0 - ink_adjusted;

    // 5. CMY channel balance (-50..=+50)
    // Cyan balance acts on Red, Magenta balance on Green, Yellow balance on Blue.
    let ch_ink = (1.0 - x_intensity).max(0.0);
    let ch_ink_adjusted = ch_ink * (1.0 + balance / 100.0);
    let x_final = 1.0 - ch_ink_adjusted;

    (x_final * 255.0 + 0.5).clamp(0.0, 255.0) as u8
}

/// Precomputes the 256-entry lookup tables for Red, Green, and Blue channels.
fn compute_lut(adj: &ColorAdjustment) -> ([u8; 256], [u8; 256], [u8; 256]) {
    let brightness = adj.brightness.clamp(-50, 50) as f32;
    let contrast = adj.contrast.clamp(-50, 50) as f32;
    let intensity = adj.intensity.clamp(-50, 50) as f32;
    let gamma = if adj.gamma.is_finite() && (1.4..=2.6).contains(&adj.gamma) {
        adj.gamma
    } else {
        2.2
    };
    let cyan = adj.cyan_balance.clamp(-50, 50) as f32;
    let magenta = adj.magenta_balance.clamp(-50, 50) as f32;
    let yellow = adj.yellow_balance.clamp(-50, 50) as f32;

    let mut lut_r = [0u8; 256];
    let mut lut_g = [0u8; 256];
    let mut lut_b = [0u8; 256];

    for i in 0..=255 {
        let v = i as u8;
        lut_r[i] = compute_channel_lut_entry(v, brightness, contrast, intensity, gamma, cyan);
        lut_g[i] = compute_channel_lut_entry(v, brightness, contrast, intensity, gamma, magenta);
        lut_b[i] = compute_channel_lut_entry(v, brightness, contrast, intensity, gamma, yellow);
    }

    (lut_r, lut_g, lut_b)
}

/// Color transformation pipeline combining optional ICC profiling with user tone controls.
pub struct ColorPipeline {
    adjustment: ColorAdjustment,
    lut_r: [u8; 256],
    lut_g: [u8; 256],
    lut_b: [u8; 256],
    transform: Option<Transform>,
}

impl std::fmt::Debug for ColorPipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColorPipeline")
            .field("adjustment", &self.adjustment)
            .field("has_icc_transform", &self.transform.is_some())
            .finish()
    }
}

impl ColorPipeline {
    /// Constructs a color pipeline from user adjustments with default sRGB pass-through.
    pub fn new(adjustment: ColorAdjustment) -> Self {
        let (lut_r, lut_g, lut_b) = compute_lut(&adjustment);
        Self {
            adjustment,
            lut_r,
            lut_g,
            lut_b,
            transform: None,
        }
    }

    /// Constructs a color pipeline with an ICC output profile.
    pub fn with_icc_profile(
        adjustment: ColorAdjustment,
        profile_bytes: &[u8],
    ) -> Result<Self, ColorError> {
        let in_profile = Profile::new_sRGB();
        let out_profile =
            Profile::new_from_slice(profile_bytes, false).ok_or(ColorError::InvalidIccProfile)?;
        let transform = Transform::new(
            &in_profile,
            &out_profile,
            DataType::RGB8,
            Intent::Perceptual,
        )
        .ok_or(ColorError::TransformCreationFailed)?;

        let (lut_r, lut_g, lut_b) = compute_lut(&adjustment);
        Ok(Self {
            adjustment,
            lut_r,
            lut_g,
            lut_b,
            transform: Some(transform),
        })
    }

    /// Returns the current color adjustment configuration.
    #[inline]
    pub fn adjustment(&self) -> &ColorAdjustment {
        &self.adjustment
    }

    /// Updates the color adjustments and recomputes the 256-entry lookup tables.
    pub fn set_adjustment(&mut self, adjustment: ColorAdjustment) {
        let (lut_r, lut_g, lut_b) = compute_lut(&adjustment);
        self.adjustment = adjustment;
        self.lut_r = lut_r;
        self.lut_g = lut_g;
        self.lut_b = lut_b;
    }

    /// Returns the precomputed 256-entry lookup table for the Red channel.
    #[inline]
    pub fn lut_r(&self) -> &[u8; 256] {
        &self.lut_r
    }

    /// Returns the precomputed 256-entry lookup table for the Green channel.
    #[inline]
    pub fn lut_g(&self) -> &[u8; 256] {
        &self.lut_g
    }

    /// Returns the precomputed 256-entry lookup table for the Blue channel.
    #[inline]
    pub fn lut_b(&self) -> &[u8; 256] {
        &self.lut_b
    }

    /// Transforms a single RGB pixel in-place and returns the resulting `(r, g, b)`.
    pub fn transform_pixel(&self, r: u8, g: u8, b: u8) -> (u8, u8, u8) {
        let mut px = [r, g, b];
        if let Some(ref xfm) = self.transform {
            xfm.apply(&mut px);
        }
        if self.adjustment.grayscale {
            let gray = rgb_to_sgray_fixed_point(px[0], px[1], px[2]);
            (
                self.lut_r[gray as usize],
                self.lut_g[gray as usize],
                self.lut_b[gray as usize],
            )
        } else {
            (
                self.lut_r[px[0] as usize],
                self.lut_g[px[1] as usize],
                self.lut_b[px[2] as usize],
            )
        }
    }

    /// Transforms an RGB scanline in-place.
    ///
    /// # Panics
    /// Panics if `rgb.len()` is not a multiple of 3.
    pub fn transform_scanline(&self, rgb: &mut [u8]) {
        assert_eq!(
            rgb.len() % 3,
            0,
            "RGB scanline length must be a multiple of 3"
        );

        if let Some(ref xfm) = self.transform {
            xfm.apply(rgb);
        }

        if self.adjustment.grayscale {
            for chunk in rgb.chunks_exact_mut(3) {
                let gray = rgb_to_sgray_fixed_point(chunk[0], chunk[1], chunk[2]);
                chunk[0] = self.lut_r[gray as usize];
                chunk[1] = self.lut_g[gray as usize];
                chunk[2] = self.lut_b[gray as usize];
            }
        } else {
            for chunk in rgb.chunks_exact_mut(3) {
                chunk[0] = self.lut_r[chunk[0] as usize];
                chunk[1] = self.lut_g[chunk[1] as usize];
                chunk[2] = self.lut_b[chunk[2] as usize];
            }
        }
    }

    /// Transforms a source scanline into a destination buffer.
    pub fn transform_scanline_to(&self, src: &[u8], dst: &mut [u8]) -> Result<(), ColorError> {
        if src.len() != dst.len() {
            return Err(ColorError::BufferLengthMismatch {
                expected: src.len(),
                actual: dst.len(),
            });
        }
        dst.copy_from_slice(src);
        self.transform_scanline(dst);
        Ok(())
    }

    /// Transforms a full RGB raster buffer in-place using Rayon parallel iterators.
    ///
    /// # Panics
    /// Panics if `rgb.len()` is not a multiple of 3.
    pub fn transform_buffer(&self, rgb: &mut [u8]) {
        assert_eq!(
            rgb.len() % 3,
            0,
            "RGB buffer length must be a multiple of 3"
        );

        if let Some(ref xfm) = self.transform {
            xfm.apply(rgb);
        }

        if self.adjustment.grayscale {
            rgb.par_chunks_exact_mut(3).for_each(|chunk| {
                let gray = rgb_to_sgray_fixed_point(chunk[0], chunk[1], chunk[2]);
                chunk[0] = self.lut_r[gray as usize];
                chunk[1] = self.lut_g[gray as usize];
                chunk[2] = self.lut_b[gray as usize];
            });
        } else {
            rgb.par_chunks_exact_mut(3).for_each(|chunk| {
                chunk[0] = self.lut_r[chunk[0] as usize];
                chunk[1] = self.lut_g[chunk[1] as usize];
                chunk[2] = self.lut_b[chunk[2] as usize];
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rgb_to_sgray_fixed_point_bounds() {
        assert_eq!(rgb_to_sgray_fixed_point(0, 0, 0), 0);
        assert_eq!(rgb_to_sgray_fixed_point(255, 255, 255), 255);
        for v in 0..=255u8 {
            assert_eq!(
                rgb_to_sgray_fixed_point(v, v, v),
                v,
                "Neutral gray {v} must map to {v}"
            );
        }

        // ITU-R BT.709 weights check: Green > Red > Blue
        let green_lum = rgb_to_sgray_fixed_point(0, 200, 0);
        let red_lum = rgb_to_sgray_fixed_point(200, 0, 0);
        let blue_lum = rgb_to_sgray_fixed_point(0, 0, 200);

        assert!(
            green_lum > red_lum,
            "Green must have higher luminance than Red"
        );
        assert!(
            red_lum > blue_lum,
            "Red must have higher luminance than Blue"
        );
    }

    #[test]
    fn test_default_adjustment_identity() {
        let pipeline = ColorPipeline::new(ColorAdjustment::default());

        for i in 0..=255 {
            assert_eq!(
                pipeline.lut_r()[i],
                i as u8,
                "Red LUT must be identity at {i}"
            );
            assert_eq!(
                pipeline.lut_g()[i],
                i as u8,
                "Green LUT must be identity at {i}"
            );
            assert_eq!(
                pipeline.lut_b()[i],
                i as u8,
                "Blue LUT must be identity at {i}"
            );
        }

        let mut data = [10, 50, 100, 200, 255, 0];
        let original = data;
        pipeline.transform_scanline(&mut data);
        assert_eq!(data, original, "Default pipeline must preserve scanlines");
    }

    #[test]
    fn test_brightness_adjustment() {
        let adj_high = ColorAdjustment {
            brightness: 50,
            ..Default::default()
        };
        let pipe_high = ColorPipeline::new(adj_high);

        let adj_low = ColorAdjustment {
            brightness: -50,
            ..Default::default()
        };
        let pipe_low = ColorPipeline::new(adj_low);

        let mid = 128;
        assert!(
            pipe_high.lut_r()[mid] > mid as u8,
            "Positive brightness must lighten values"
        );
        assert!(
            pipe_low.lut_r()[mid] < mid as u8,
            "Negative brightness must darken values"
        );
    }

    #[test]
    fn test_contrast_adjustment() {
        let adj = ColorAdjustment {
            contrast: 50,
            ..Default::default()
        };
        let pipe = ColorPipeline::new(adj);

        // Above midtone should get brighter, below midtone should get darker
        assert!(pipe.lut_r()[192] > 192);
        assert!(pipe.lut_r()[64] < 64);
    }

    #[test]
    fn test_gamma_adjustment() {
        let adj_light = ColorAdjustment {
            gamma: 1.4,
            ..Default::default()
        };
        let pipe_light = ColorPipeline::new(adj_light);

        let adj_dark = ColorAdjustment {
            gamma: 2.6,
            ..Default::default()
        };
        let pipe_dark = ColorPipeline::new(adj_dark);

        // Lower gamma lightens midtones, higher gamma darkens midtones
        assert!(pipe_light.lut_r()[128] > 128);
        assert!(pipe_dark.lut_r()[128] < 128);
    }

    #[test]
    fn test_intensity_adjustment() {
        let adj_intense = ColorAdjustment {
            intensity: 50,
            ..Default::default()
        };
        let pipe_intense = ColorPipeline::new(adj_intense);

        // High intensity deposits more ink -> darker RGB for midtones
        assert!(pipe_intense.lut_r()[128] < 128);

        // Pure white paper (255) must remain untouched white (255)
        assert_eq!(pipe_intense.lut_r()[255], 255);
        assert_eq!(pipe_intense.lut_g()[255], 255);
        assert_eq!(pipe_intense.lut_b()[255], 255);
    }

    #[test]
    fn test_channel_balance_adjustments() {
        let adj = ColorAdjustment {
            cyan_balance: 50,
            magenta_balance: -50,
            yellow_balance: 0,
            ..Default::default()
        };
        let pipe = ColorPipeline::new(adj);

        // Cyan balance +50 increases cyan ink -> lowers Red channel
        assert!(pipe.lut_r()[128] < 128);
        // Magenta balance -50 decreases magenta ink -> raises Green channel
        assert!(pipe.lut_g()[128] > 128);
        // Yellow balance 0 -> Blue channel unchanged
        assert_eq!(pipe.lut_b()[128], 128);

        // White paper remains white on all channels
        assert_eq!(pipe.lut_r()[255], 255);
        assert_eq!(pipe.lut_g()[255], 255);
        assert_eq!(pipe.lut_b()[255], 255);
    }

    #[test]
    fn test_grayscale_mode() {
        let adj = ColorAdjustment {
            grayscale: true,
            ..Default::default()
        };
        let pipe = ColorPipeline::new(adj);

        let (r, g, b) = pipe.transform_pixel(255, 0, 0);
        // Gray value should be identical across R, G, B
        assert_eq!(r, g);
        assert_eq!(g, b);
        assert_eq!(r, rgb_to_sgray_fixed_point(255, 0, 0));
    }

    #[test]
    fn test_parallel_buffer_matches_scanline() {
        let adj = ColorAdjustment {
            brightness: 10,
            contrast: -15,
            ..Default::default()
        };
        let pipe = ColorPipeline::new(adj);

        let mut buf_seq = vec![0u8; 3000];
        for (i, byte) in buf_seq.iter_mut().enumerate() {
            *byte = (i * 17 % 256) as u8;
        }
        let mut buf_par = buf_seq.clone();

        pipe.transform_scanline(&mut buf_seq);
        pipe.transform_buffer(&mut buf_par);

        assert_eq!(buf_seq, buf_par);
    }

    #[test]
    fn test_invalid_icc_profile_handling() {
        let bogus_bytes = b"not a real icc profile header";
        let res = ColorPipeline::with_icc_profile(ColorAdjustment::default(), bogus_bytes);
        assert!(res.is_err());
        match res.unwrap_err() {
            ColorError::InvalidIccProfile => (),
            other => panic!("Expected InvalidIccProfile, got {other:?}"),
        }
    }
}
