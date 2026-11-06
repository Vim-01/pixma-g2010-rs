//! Histogram analysis and parametric tone curve mapping.
//!
//! Provides:
//! - 256-bin RGB and Luminance histogram computation and percentile analysis.
//! - Monotone Cubic Spline (Fritsch-Carlson) tone curve interpolation.
//! - Black point, white point, and midtone gamma adjustment lookup tables (LUT).

use image::{ImageBuffer, Rgb, RgbImage};

use crate::errors::ImageProcError;

/// 256-bin histogram for Red, Green, Blue, and Luminance channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RgbHistogram {
    pub r: [u32; 256],
    pub g: [u32; 256],
    pub b: [u32; 256],
    pub luma: [u32; 256],
    pub total_pixels: u64,
}

impl RgbHistogram {
    /// Computes the 256-bin channel and luminance histograms for the given image.
    pub fn from_image(img: &RgbImage) -> Self {
        let mut r = [0u32; 256];
        let mut g = [0u32; 256];
        let mut b = [0u32; 256];
        let mut luma = [0u32; 256];
        let mut total_pixels = 0u64;

        for p in img.pixels() {
            let pr = p[0];
            let pg = p[1];
            let pb = p[2];
            let lum = (0.299 * pr as f32 + 0.587 * pg as f32 + 0.114 * pb as f32).round() as usize;

            r[pr as usize] += 1;
            g[pg as usize] += 1;
            b[pb as usize] += 1;
            luma[lum.min(255)] += 1;
            total_pixels += 1;
        }

        Self {
            r,
            g,
            b,
            luma,
            total_pixels,
        }
    }

    /// Computes the luminance value at the specified percentile `pct` in `[0.0, 100.0]`.
    pub fn luma_percentile(&self, pct: f64) -> u8 {
        Self::calc_percentile(&self.luma, self.total_pixels, pct)
    }

    /// Computes the channel value at the specified percentile `pct` in `[0.0, 100.0]`.
    pub fn calc_percentile(bins: &[u32; 256], total: u64, pct: f64) -> u8 {
        if total == 0 {
            return 0;
        }

        let target = ((pct.clamp(0.0, 100.0) / 100.0) * total as f64).round() as u64;
        let mut acc = 0u64;
        for (i, &count) in bins.iter().enumerate() {
            acc += count as u64;
            if acc >= target {
                return i as u8;
            }
        }
        255
    }

    /// Calculates automatic levels `(black_point, white_point)` by clipping the specified
    /// percentage of shadows and highlights (typically `clip_percent = 0.5`).
    pub fn auto_levels_points(&self, clip_percent: f32) -> (u8, u8) {
        let bp = self.luma_percentile(clip_percent as f64);
        let wp = self.luma_percentile((100.0 - clip_percent) as f64);
        if bp >= wp {
            (0, 255)
        } else {
            (bp, wp)
        }
    }

    /// Calculates the mean luminance across all pixels.
    pub fn mean_luma(&self) -> f64 {
        if self.total_pixels == 0 {
            return 0.0;
        }
        let mut sum = 0.0f64;
        for (i, &count) in self.luma.iter().enumerate() {
            sum += i as f64 * count as f64;
        }
        sum / self.total_pixels as f64
    }
}

/// 256-entry Look-Up Table (LUT) tone curve mapping for high-speed color transformations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToneCurve {
    lut: [u8; 256],
}

impl ToneCurve {
    /// Creates an identity tone curve where `f(x) = x`.
    pub fn identity() -> Self {
        let mut lut = [0u8; 256];
        for (i, val) in lut.iter_mut().enumerate() {
            *val = i as u8;
        }
        Self { lut }
    }

    /// Creates a gamma correction curve: `y = 255 * (x / 255)^(1 / gamma)`.
    pub fn from_gamma(gamma: f32) -> Self {
        let g = if gamma <= 0.0 { 1.0 } else { gamma };
        let inv_gamma = 1.0 / g;

        let mut lut = [0u8; 256];
        for (i, val) in lut.iter_mut().enumerate() {
            let norm = i as f32 / 255.0;
            *val = (norm.powf(inv_gamma) * 255.0).clamp(0.0, 255.0).round() as u8;
        }
        Self { lut }
    }

    /// Creates a tone curve with explicit black point, white point, and midtone gamma.
    ///
    /// Values `<= black_point` are clamped to 0, values `>= white_point` are clamped to 255,
    /// and intermediate values follow the specified midtone gamma power function.
    pub fn from_black_white_points(
        black_point: u8,
        white_point: u8,
        gamma: f32,
    ) -> Result<Self, ImageProcError> {
        if black_point >= white_point {
            return Err(ImageProcError::InvalidControlPoints(format!(
                "Black point ({black_point}) must be strictly less than white point ({white_point})"
            )));
        }

        let g = if gamma <= 0.0 { 1.0 } else { gamma };
        let inv_gamma = 1.0 / g;
        let range = (white_point - black_point) as f32;

        let mut lut = [0u8; 256];
        for (i_usize, val) in lut.iter_mut().enumerate() {
            let i = i_usize as u8;
            *val = if i <= black_point {
                0
            } else if i >= white_point {
                255
            } else {
                let norm = (i - black_point) as f32 / range;
                (norm.powf(inv_gamma) * 255.0).clamp(0.0, 255.0).round() as u8
            };
        }

        Ok(Self { lut })
    }

    /// Generates a tone curve using Fritsch-Carlson monotone cubic spline interpolation
    /// from control points `[(x0, y0), (x1, y1), ..., (xn, yn)]`.
    ///
    /// Control points must begin at `x=0`, end at `x=255`, have strictly increasing `x`,
    /// and non-decreasing `y` to preserve monotonicity.
    pub fn from_spline_control_points(points: &[(u8, u8)]) -> Result<Self, ImageProcError> {
        if points.len() < 2 {
            return Err(ImageProcError::InvalidControlPoints(
                "At least 2 control points required".into(),
            ));
        }

        if points[0].0 != 0 || points.last().unwrap().0 != 255 {
            return Err(ImageProcError::InvalidControlPoints(
                "Control points must span from x=0 to x=255".into(),
            ));
        }

        // Validate strictly increasing x and monotonic y
        for i in 0..points.len() - 1 {
            if points[i].0 >= points[i + 1].0 {
                return Err(ImageProcError::InvalidControlPoints(format!(
                    "Control point x coordinates must be strictly increasing: {} >= {}",
                    points[i].0,
                    points[i + 1].0
                )));
            }
            if points[i].1 > points[i + 1].1 {
                return Err(ImageProcError::InvalidControlPoints(format!(
                    "Control point y coordinates must be non-decreasing: {} > {}",
                    points[i].1,
                    points[i + 1].1
                )));
            }
        }

        let n = points.len();
        let xs: Vec<f32> = points.iter().map(|&(x, _)| x as f32).collect();
        let ys: Vec<f32> = points.iter().map(|&(_, y)| y as f32).collect();

        // 1. Secant slopes
        let mut deltas = vec![0.0f32; n - 1];
        for k in 0..n - 1 {
            deltas[k] = (ys[k + 1] - ys[k]) / (xs[k + 1] - xs[k]);
        }

        // 2. Initial tangents
        let mut d = vec![0.0f32; n];
        d[0] = deltas[0];
        d[n - 1] = deltas[n - 2];
        for k in 1..n - 1 {
            d[k] = (deltas[k - 1] + deltas[k]) / 2.0;
        }

        // 3. Fritsch-Carlson monotonicity enforcement
        for k in 0..n - 1 {
            if deltas[k].abs() < 1e-6 {
                d[k] = 0.0;
                d[k + 1] = 0.0;
            } else {
                let alpha = d[k] / deltas[k];
                let beta = d[k + 1] / deltas[k];
                let hyp = alpha * alpha + beta * beta;
                if hyp > 9.0 {
                    let tau = 3.0 / hyp.sqrt();
                    d[k] = tau * alpha * deltas[k];
                    d[k + 1] = tau * beta * deltas[k];
                }
            }
        }

        // 4. Evaluate cubic Hermite spline for all 256 LUT entries
        let mut lut = [0u8; 256];
        for (xi, val) in lut.iter_mut().enumerate() {
            let x = xi as f32;

            // Find interval
            let mut k = 0;
            while k < n - 2 && x > xs[k + 1] {
                k += 1;
            }

            let h = xs[k + 1] - xs[k];
            let t = (x - xs[k]) / h;
            let t2 = t * t;
            let t3 = t2 * t;

            let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
            let h10 = t3 - 2.0 * t2 + t;
            let h01 = -2.0 * t3 + 3.0 * t2;
            let h11 = t3 - t2;

            let y = h00 * ys[k] + h10 * h * d[k] + h01 * ys[k + 1] + h11 * h * d[k + 1];
            *val = y.clamp(0.0, 255.0).round() as u8;
        }

        Ok(Self { lut })
    }

    /// Returns a reference to the internal 256-byte look-up table.
    pub fn lut(&self) -> &[u8; 256] {
        &self.lut
    }

    /// Applies the tone curve to all pixels of the input image, returning a new image.
    pub fn apply_to_image(&self, img: &RgbImage) -> RgbImage {
        let mut out = ImageBuffer::new(img.width(), img.height());
        for (p_in, p_out) in img.pixels().zip(out.pixels_mut()) {
            let r = self.lut[p_in[0] as usize];
            let g = self.lut[p_in[1] as usize];
            let b = self.lut[p_in[2] as usize];
            *p_out = Rgb([r, g, b]);
        }
        out
    }

    /// In-place applies the look-up table to an arbitrary byte slice.
    pub fn apply_to_slice(&self, slice: &mut [u8]) {
        for b in slice.iter_mut() {
            *b = self.lut[*b as usize];
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_tone_curve_leaves_pixels_unchanged() {
        let curve = ToneCurve::identity();
        for i in 0..=255 {
            assert_eq!(curve.lut()[i], i as u8);
        }
    }

    #[test]
    fn gamma_curve_brightens_midtones() {
        let curve = ToneCurve::from_gamma(2.2);
        // At gamma 2.2, midtone x=128 should brighten to > 128
        assert!(curve.lut()[128] > 128);
        assert_eq!(curve.lut()[0], 0);
        assert_eq!(curve.lut()[255], 255);
    }

    #[test]
    fn black_white_points_clamps_endpoints() {
        let curve = ToneCurve::from_black_white_points(30, 220, 1.0).unwrap();
        assert_eq!(curve.lut()[0], 0);
        assert_eq!(curve.lut()[30], 0);
        assert_eq!(curve.lut()[220], 255);
        assert_eq!(curve.lut()[255], 255);
        // Middle should be around 128
        assert!((curve.lut()[125] as i32 - 128).abs() < 5);
    }

    #[test]
    fn spline_curve_is_strictly_monotonic() {
        let points = [(0, 0), (64, 40), (128, 160), (192, 220), (255, 255)];
        let curve = ToneCurve::from_spline_control_points(&points).unwrap();

        for i in 0..255 {
            assert!(
                curve.lut()[i] <= curve.lut()[i + 1],
                "Monotonicity violated at {i}: {} > {}",
                curve.lut()[i],
                curve.lut()[i + 1]
            );
        }
    }

    #[test]
    fn histogram_calculates_percentiles() {
        let mut img = RgbImage::new(10, 10);
        for (i, p) in img.pixels_mut().enumerate() {
            let v = (i * 2) as u8;
            *p = Rgb([v, v, v]);
        }

        let hist = RgbHistogram::from_image(&img);
        assert_eq!(hist.total_pixels, 100);
        let p50 = hist.luma_percentile(50.0);
        assert!((p50 as i32 - 100).abs() < 5);
        assert!(hist.mean_luma() > 80.0 && hist.mean_luma() < 120.0);
    }

    #[test]
    fn rejects_invalid_tone_curve_configurations() {
        // Black point >= white point
        assert!(ToneCurve::from_black_white_points(100, 100, 1.0).is_err());
        assert!(ToneCurve::from_black_white_points(150, 100, 1.0).is_err());

        // Less than 2 points
        assert!(ToneCurve::from_spline_control_points(&[(0, 0)]).is_err());

        // Doesn't start at x=0
        assert!(ToneCurve::from_spline_control_points(&[(10, 0), (255, 255)]).is_err());

        // Doesn't end at x=255
        assert!(ToneCurve::from_spline_control_points(&[(0, 0), (200, 255)]).is_err());

        // Non-increasing x
        assert!(ToneCurve::from_spline_control_points(&[
            (0, 0),
            (100, 100),
            (90, 200),
            (255, 255)
        ])
        .is_err());

        // Decreasing y (violates monotonicity)
        assert!(ToneCurve::from_spline_control_points(&[
            (0, 0),
            (100, 150),
            (200, 120),
            (255, 255)
        ])
        .is_err());
    }

    #[test]
    fn applies_tone_curve_to_byte_slice() {
        let curve = ToneCurve::from_gamma(2.0);
        let mut data = [0u8, 128, 255];
        curve.apply_to_slice(&mut data);
        assert_eq!(data[0], 0);
        assert!(data[1] > 128);
        assert_eq!(data[2], 255);
    }
}
