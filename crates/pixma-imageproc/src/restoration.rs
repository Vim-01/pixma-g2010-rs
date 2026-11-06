//! Pure-Rust scanner image restoration filters matching Canon ScanGear Pro.
//!
//! Provides high-fidelity algorithms for:
//! - Unsharp Masking (edge sharpening via Gaussian blur subtraction).
//! - Descreening (halftone moiré pattern removal via 2D Fast Fourier Transform on luminance).
//! - Dust and Scratch Removal (morphological median outlier filtering).
//! - Gutter Shadow Correction (book-spine illumination gradient normalization).

use image::{ImageBuffer, Rgb, RgbImage};
use rayon::prelude::*;
use rustfft::num_complex::Complex;
use rustfft::FftPlanner;

/// Sharpens edges by subtracting a Gaussian-blurred low-pass copy from the original image.
///
/// Formula: `I_sharp = I + amount * (I - GaussianBlur(I, sigma))`
pub fn unsharp_mask(img: &RgbImage, sigma: f32, amount: f32) -> RgbImage {
    let width = img.width();
    let height = img.height();

    if width == 0 || height == 0 || sigma <= 0.0 || amount <= 0.0 {
        return img.clone();
    }

    // Generate 1D Gaussian kernel
    let radius = ((3.0 * sigma).ceil() as usize).clamp(1, 25);
    let mut kernel = Vec::with_capacity(2 * radius + 1);
    let mut sum = 0.0f32;
    let two_sigma_sq = 2.0 * sigma * sigma;

    for i in -(radius as isize)..=(radius as isize) {
        let w = (-(i * i) as f32 / two_sigma_sq).exp();
        kernel.push(w);
        sum += w;
    }
    for w in &mut kernel {
        *w /= sum;
    }

    let w_usize = width as usize;
    let h_usize = height as usize;

    // Pass 1: Horizontal Gaussian blur (parallelized across rows)
    let mut horiz_blur = vec![[0.0f32; 3]; w_usize * h_usize];
    horiz_blur
        .par_chunks_exact_mut(w_usize)
        .enumerate()
        .for_each(|(y, row_out)| {
            for (x, out_acc) in row_out.iter_mut().enumerate() {
                let mut acc = [0.0f32; 3];
                for (k_idx, &weight) in kernel.iter().enumerate() {
                    let k_offset = k_idx as isize - radius as isize;
                    let src_x = (x as isize + k_offset).clamp(0, w_usize as isize - 1) as usize;
                    let p = img.get_pixel(src_x as u32, y as u32);
                    acc[0] += p[0] as f32 * weight;
                    acc[1] += p[1] as f32 * weight;
                    acc[2] += p[2] as f32 * weight;
                }
                *out_acc = acc;
            }
        });

    // Pass 2: Vertical Gaussian blur + unsharp mask sharpening (parallelized across rows)
    let mut output = ImageBuffer::new(width, height);
    let out_raw = output.as_flat_samples_mut().samples;

    out_raw
        .par_chunks_exact_mut(w_usize * 3)
        .enumerate()
        .for_each(|(y, row_bytes)| {
            for x in 0..w_usize {
                let mut blurred = [0.0f32; 3];
                for (k_idx, &weight) in kernel.iter().enumerate() {
                    let k_offset = k_idx as isize - radius as isize;
                    let src_y = (y as isize + k_offset).clamp(0, h_usize as isize - 1) as usize;
                    let sample = horiz_blur[src_y * w_usize + x];
                    blurred[0] += sample[0] * weight;
                    blurred[1] += sample[1] * weight;
                    blurred[2] += sample[2] * weight;
                }

                let orig = img.get_pixel(x as u32, y as u32);
                let r_sharp = orig[0] as f32 + amount * (orig[0] as f32 - blurred[0]);
                let g_sharp = orig[1] as f32 + amount * (orig[1] as f32 - blurred[1]);
                let b_sharp = orig[2] as f32 + amount * (orig[2] as f32 - blurred[2]);

                let offset = x * 3;
                row_bytes[offset] = r_sharp.clamp(0.0, 255.0).round() as u8;
                row_bytes[offset + 1] = g_sharp.clamp(0.0, 255.0).round() as u8;
                row_bytes[offset + 2] = b_sharp.clamp(0.0, 255.0).round() as u8;
            }
        });

    output
}

/// Descreens scanned print media by performing a 2D Fast Fourier Transform on the luminance
/// channel, attenuating periodic halftone carrier peaks and high-frequency moiré interference,
/// and inverse-transforming back to spatial RGB.
pub fn descreen_moire_2d_fft(img: &RgbImage) -> RgbImage {
    let width = img.width() as usize;
    let height = img.height() as usize;

    if width < 8 || height < 8 {
        return img.clone();
    }

    // 1. Convert RGB to YCbCr color planes
    let total_px = width * height;
    let mut lum = Vec::with_capacity(total_px);
    let mut cb_plane = Vec::with_capacity(total_px);
    let mut cr_plane = Vec::with_capacity(total_px);

    for p in img.pixels() {
        let r = p[0] as f32;
        let g = p[1] as f32;
        let b = p[2] as f32;

        let y = 0.299 * r + 0.587 * g + 0.114 * b;
        let cb = -0.168736 * r - 0.331264 * g + 0.5 * b + 128.0;
        let cr = 0.5 * r - 0.418688 * g - 0.081312 * b + 128.0;

        lum.push(Complex::new(y, 0.0f32));
        cb_plane.push(cb);
        cr_plane.push(cr);
    }

    // 2. 2D Forward FFT on Luminance
    let mut planner = FftPlanner::new();
    let fft_row = planner.plan_fft_forward(width);
    let fft_col = planner.plan_fft_forward(height);

    // Row-wise forward FFT
    lum.par_chunks_exact_mut(width).for_each(|row| {
        fft_row.process(row);
    });

    // Column-wise forward FFT (scratch transpose buffer)
    let mut lum_transposed = vec![Complex::new(0.0f32, 0.0f32); total_px];
    for y in 0..height {
        for x in 0..width {
            lum_transposed[x * height + y] = lum[y * width + x];
        }
    }
    lum_transposed.par_chunks_exact_mut(height).for_each(|col| {
        fft_col.process(col);
    });

    // 3. Notch and Low-Pass Frequency Domain Filter
    // In frequency domain: u in [0, width), v in [0, height)
    // DC is at u=0, v=0. Radial distance normalized to [0, 1].
    let half_w = (width / 2).max(1) as f32;
    let half_h = (height / 2).max(1) as f32;

    // Collect high-frequency magnitude statistics to detect carrier spikes
    let mut sum_hf_mag = 0.0f64;
    let mut sum_hf_sq = 0.0f64;
    let mut hf_count = 0usize;

    for x in 0..width {
        let kx = (x as f32).min((width - x) as f32) / half_w;
        for y in 0..height {
            let ky = (y as f32).min((height - y) as f32) / half_h;
            let r = (kx * kx + ky * ky).sqrt();
            if r >= 0.10 {
                let mag = lum_transposed[x * height + y].norm() as f64;
                sum_hf_mag += mag;
                sum_hf_sq += mag * mag;
                hf_count += 1;
            }
        }
    }

    let mean_mag = if hf_count > 0 {
        sum_hf_mag / hf_count as f64
    } else {
        0.0
    };
    let variance = if hf_count > 0 {
        (sum_hf_sq / hf_count as f64) - (mean_mag * mean_mag)
    } else {
        0.0
    };
    let std_dev = variance.max(0.0).sqrt();
    let carrier_threshold = (mean_mag + 2.5 * std_dev) as f32;

    // Apply frequency filtering
    for x in 0..width {
        let kx = (x as f32).min((width - x) as f32) / half_w;
        for y in 0..height {
            let ky = (y as f32).min((height - y) as f32) / half_h;
            let r = (kx * kx + ky * ky).sqrt();

            if r < 0.08 {
                // Keep DC and low-frequency components unaltered
                continue;
            }

            let idx = x * height + y;
            let mag = lum_transposed[idx].norm();

            // Butterworth smooth low-pass roll-off for print halftone carrier region
            let butterworth = 1.0 / (1.0 + (r / 0.65).powi(4));

            // Peak notch attenuation if this frequency exceeds the carrier spike threshold
            let notch = if mag > carrier_threshold && carrier_threshold > 0.0 {
                (carrier_threshold / mag).max(0.10)
            } else {
                1.0
            };

            let gain = butterworth * notch;
            lum_transposed[idx] *= gain;
        }
    }

    // 4. 2D Inverse FFT
    let ifft_col = planner.plan_fft_inverse(height);
    let ifft_row = planner.plan_fft_inverse(width);

    // Column-wise inverse FFT
    lum_transposed.par_chunks_exact_mut(height).for_each(|col| {
        ifft_col.process(col);
    });

    // Transpose back
    for x in 0..width {
        for y in 0..height {
            lum[y * width + x] = lum_transposed[x * height + y];
        }
    }

    // Row-wise inverse FFT
    lum.par_chunks_exact_mut(width).for_each(|row| {
        ifft_row.process(row);
    });

    // 5. Reconstruct RGB from reconstructed luminance and preserved chrominance
    let scale = 1.0 / (total_px as f32);
    let mut out_img = ImageBuffer::new(width as u32, height as u32);

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let y_rec = (lum[idx].re * scale).clamp(0.0, 255.0);
            let cb = cb_plane[idx] - 128.0;
            let cr = cr_plane[idx] - 128.0;

            let r = (y_rec + 1.402 * cr).clamp(0.0, 255.0).round() as u8;
            let g = (y_rec - 0.344136 * cb - 0.714136 * cr)
                .clamp(0.0, 255.0)
                .round() as u8;
            let b = (y_rec + 1.772 * cb).clamp(0.0, 255.0).round() as u8;

            out_img.put_pixel(x as u32, y as u32, Rgb([r, g, b]));
        }
    }

    out_img
}

/// Removes isolated dust specks and scratches on the flatbed glass using a morphological
/// local median outlier detector.
///
/// Pixels that deviate from their immediate neighborhood median by more than `threshold`
/// are replaced with the neighborhood median; normal pixels are preserved untouched.
pub fn remove_dust_and_scratches(img: &RgbImage, radius: u32, threshold: u8) -> RgbImage {
    let width = img.width();
    let height = img.height();

    if width == 0 || height == 0 || radius == 0 || threshold == 0 {
        return img.clone();
    }

    let r = radius.clamp(1, 8) as i32;
    let mut output = ImageBuffer::new(width, height);
    let w_i32 = width as i32;
    let h_i32 = height as i32;
    let thresh_f = threshold as i32;

    let out_raw = output.as_flat_samples_mut().samples;
    out_raw
        .par_chunks_exact_mut((width * 3) as usize)
        .enumerate()
        .for_each(|(y_idx, row_bytes)| {
            let y = y_idx as i32;
            let mut neighbors_r = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
            let mut neighbors_g = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);
            let mut neighbors_b = Vec::with_capacity(((2 * r + 1) * (2 * r + 1)) as usize);

            for x in 0..w_i32 {
                neighbors_r.clear();
                neighbors_g.clear();
                neighbors_b.clear();

                // Collect neighborhood excluding center pixel
                for dy in -r..=r {
                    let ny = y + dy;
                    if ny < 0 || ny >= h_i32 {
                        continue;
                    }
                    for dx in -r..=r {
                        if dx == 0 && dy == 0 {
                            continue; // Skip center pixel
                        }
                        let nx = x + dx;
                        if nx < 0 || nx >= w_i32 {
                            continue;
                        }

                        let p = img.get_pixel(nx as u32, ny as u32);
                        neighbors_r.push(p[0]);
                        neighbors_g.push(p[1]);
                        neighbors_b.push(p[2]);
                    }
                }

                let orig = img.get_pixel(x as u32, y as u32);
                if neighbors_r.len() >= 3 {
                    neighbors_r.sort_unstable();
                    neighbors_g.sort_unstable();
                    neighbors_b.sort_unstable();

                    let med_r = neighbors_r[neighbors_r.len() / 2];
                    let med_g = neighbors_g[neighbors_g.len() / 2];
                    let med_b = neighbors_b[neighbors_b.len() / 2];

                    let dr = (orig[0] as i32 - med_r as i32).abs();
                    let dg = (orig[1] as i32 - med_g as i32).abs();
                    let db = (orig[2] as i32 - med_b as i32).abs();

                    let offset = (x as usize) * 3;
                    if dr.max(dg).max(db) > thresh_f {
                        // Dust or scratch outlier detected: replace with local median
                        row_bytes[offset] = med_r;
                        row_bytes[offset + 1] = med_g;
                        row_bytes[offset + 2] = med_b;
                    } else {
                        // Preserved untouched
                        row_bytes[offset] = orig[0];
                        row_bytes[offset + 1] = orig[1];
                        row_bytes[offset + 2] = orig[2];
                    }
                } else {
                    let offset = (x as usize) * 3;
                    row_bytes[offset] = orig[0];
                    row_bytes[offset + 1] = orig[1];
                    row_bytes[offset + 2] = orig[2];
                }
            }
        });

    output
}

/// Compensates for dark book-spine shadow gradients on flatbed scans using column-wise
/// illumination gain normalization.
pub fn correct_gutter_shadow(img: &RgbImage, spine_x: u32, shadow_width: u32) -> RgbImage {
    let width = img.width();
    let height = img.height();

    if width == 0 || height == 0 || shadow_width == 0 {
        return img.clone();
    }

    let spine = spine_x as i64;
    let sh_w = shadow_width as f32;

    // Estimate background paper brightness outside the shadow zone
    let sample_x_left = (spine - shadow_width as i64).clamp(0, width as i64 - 1) as u32;
    let sample_x_right = (spine + shadow_width as i64).clamp(0, width as i64 - 1) as u32;

    let sample_col_bg = |col_x: u32| -> f32 {
        let mut lumas = Vec::with_capacity(height as usize);
        for y in 0..height {
            let p = img.get_pixel(col_x, y);
            let luma = 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
            lumas.push(luma);
        }
        lumas.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        // Take 80th percentile to represent paper background
        let idx = (lumas.len() * 80) / 100;
        lumas[idx.min(lumas.len() - 1)]
    };

    let bg_ref = (sample_col_bg(sample_x_left) + sample_col_bg(sample_x_right)) / 2.0;
    let spine_bg = sample_col_bg(spine_x.clamp(0, width - 1));

    let max_boost = if spine_bg > 10.0 {
        (bg_ref / spine_bg).clamp(1.0, 3.0)
    } else {
        1.5
    };

    let mut output = ImageBuffer::new(width, height);
    for x in 0..width {
        let dist = (x as i64 - spine).abs() as f32;
        let gain = if dist < sh_w {
            let tau = dist / sh_w; // 0.0 at spine, 1.0 at boundary
            let shadow_profile = (1.0 - tau) * (1.0 - tau);
            1.0 + (max_boost - 1.0) * shadow_profile
        } else {
            1.0
        };

        for y in 0..height {
            let p = img.get_pixel(x, y);
            let r = (p[0] as f32 * gain).clamp(0.0, 255.0).round() as u8;
            let g = (p[1] as f32 * gain).clamp(0.0, 255.0).round() as u8;
            let b = (p[2] as f32 * gain).clamp(0.0, 255.0).round() as u8;
            output.put_pixel(x, y, Rgb([r, g, b]));
        }
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unsharp_mask_enhances_contrast() {
        // Create an image with an edge: left half black, right half white
        let mut img = RgbImage::new(16, 16);
        for y in 0..16 {
            for x in 0..8 {
                img.put_pixel(x, y, Rgb([50, 50, 50]));
            }
            for x in 8..16 {
                img.put_pixel(x, y, Rgb([200, 200, 200]));
            }
        }

        let sharpened = unsharp_mask(&img, 1.0, 1.0);
        assert_eq!(sharpened.width(), 16);
        assert_eq!(sharpened.height(), 16);

        // At the edge (x=7 vs x=8), contrast should be heightened:
        // x=7 should be <= original 50, x=8 should be >= original 200
        let p_edge_dark = sharpened.get_pixel(7, 8);
        let p_edge_bright = sharpened.get_pixel(8, 8);
        assert!(p_edge_dark[0] <= 50);
        assert!(p_edge_bright[0] >= 200);
    }

    #[test]
    fn remove_dust_replaces_isolated_speck() {
        // Uniform white image with a single black dust speck at (5, 5)
        let mut img = RgbImage::from_pixel(12, 12, Rgb([255, 255, 255]));
        img.put_pixel(5, 5, Rgb([10, 10, 10])); // Dust speck

        let cleaned = remove_dust_and_scratches(&img, 2, 40);
        assert_eq!(cleaned.get_pixel(5, 5), &Rgb([255, 255, 255]));
        // Non-dust pixel remains white
        assert_eq!(cleaned.get_pixel(0, 0), &Rgb([255, 255, 255]));
    }

    #[test]
    fn gutter_shadow_increases_brightness_at_spine() {
        let mut img = RgbImage::new(20, 10);
        // Dim region near spine x=10
        for y in 0..10 {
            for x in 0..20 {
                let dist = (x as i32 - 10).abs() as f32;
                let factor = (dist / 10.0).clamp(0.4, 1.0);
                let val = (200.0 * factor) as u8;
                img.put_pixel(x, y, Rgb([val, val, val]));
            }
        }

        let corrected = correct_gutter_shadow(&img, 10, 8);
        assert_eq!(corrected.width(), 20);
        // Pixel at spine should be brighter after correction
        let orig_spine = img.get_pixel(10, 5)[0];
        let corr_spine = corrected.get_pixel(10, 5)[0];
        assert!(corr_spine > orig_spine);
    }

    #[test]
    fn descreen_moire_2d_fft_preserves_dimensions() {
        let mut img = RgbImage::new(32, 32);
        // Add periodic high-frequency pattern simulating halftone screen
        for y in 0..32 {
            for x in 0..32 {
                let wave = (((x * 4) as f32).sin() * 50.0 + 128.0) as u8;
                img.put_pixel(x, y, Rgb([wave, wave, wave]));
            }
        }

        let descreened = descreen_moire_2d_fft(&img);
        assert_eq!(descreened.width(), 32);
        assert_eq!(descreened.height(), 32);
    }

    #[test]
    fn unsharp_mask_zero_parameters_is_noop() {
        let img = RgbImage::from_pixel(10, 10, Rgb([100, 150, 200]));
        let out = unsharp_mask(&img, 0.0, 1.0);
        assert_eq!(out, img);
        let out2 = unsharp_mask(&img, 1.0, 0.0);
        assert_eq!(out2, img);
    }

    #[test]
    fn remove_dust_handles_bright_speck_on_dark() {
        let mut img = RgbImage::from_pixel(12, 12, Rgb([20, 20, 20]));
        img.put_pixel(6, 6, Rgb([240, 240, 240])); // White speck on black

        let cleaned = remove_dust_and_scratches(&img, 2, 40);
        assert_eq!(cleaned.get_pixel(6, 6), &Rgb([20, 20, 20]));
    }

    #[test]
    fn correct_gutter_shadow_zero_width_is_noop() {
        let img = RgbImage::from_pixel(10, 10, Rgb([100, 100, 100]));
        let out = correct_gutter_shadow(&img, 5, 0);
        assert_eq!(out, img);
    }

    #[test]
    fn descreen_tiny_image_is_noop() {
        let img = RgbImage::from_pixel(4, 4, Rgb([50, 60, 70]));
        let out = descreen_moire_2d_fft(&img);
        assert_eq!(out, img);
    }
}
