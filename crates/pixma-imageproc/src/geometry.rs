//! Document geometry analysis, auto-multi-crop, and deskew algorithms.
//!
//! Provides pure-Rust implementations for:
//! - Auto Multi-Crop: Automatically segments multiple photos, documents, or receipts
//!   placed simultaneously on the flatbed glass into individual cropped images.
//! - Skew Angle Estimation: Detects document rotation angle via horizontal projection profile variance.
//! - Deskew: Bilinearly rotates the image to align text and borders horizontally.

use image::{ImageBuffer, Rgb, RgbImage};
use pixma_core::scan_params::ScanGeometry;

/// Segments multiple distinct documents or photos scattered on the flatbed platen.
///
/// Uses Otsu adaptive luminance thresholding to distinguish document foreground from
/// the flatbed cover background, extracts connected components, and returns each cropped
/// sub-image along with its bounding box `ScanGeometry`.
pub fn auto_multi_crop(
    img: &RgbImage,
    background_threshold: u8,
    min_area_px: u32,
) -> Vec<(ScanGeometry, RgbImage)> {
    let width = img.width();
    let height = img.height();

    if width == 0 || height == 0 {
        return Vec::new();
    }

    // Determine threshold: if 0, compute optimal threshold via Otsu's method
    let threshold = if background_threshold == 0 {
        compute_otsu_threshold(img)
    } else {
        background_threshold
    };

    // Determine whether platen background is bright (white lid closed) or dark (lid open)
    let corner_luma = compute_corner_luminance(img);
    let p85_luma = compute_percentile_luminance(img, 0.85);
    let is_bright_background = if background_threshold == 0 {
        p85_luma > 180
    } else {
        corner_luma > 128.0
    };
    let effective_bright_thresh = if background_threshold == 0 && is_bright_background {
        threshold.max(p85_luma.saturating_sub(28))
    } else {
        threshold
    };
    let border_bright_thresh = p85_luma.saturating_sub(55);

    // Downsample for high-speed connected component analysis on large scans
    let max_dim = width.max(height);
    let scale = (max_dim / 400).max(1);
    let grid_w = (width / scale).max(1);
    let grid_h = (height / scale).max(1);
    let rim_x = if background_threshold == 0 {
        (width / 80).max(1)
    } else {
        0
    };
    let rim_y = if background_threshold == 0 {
        (height / 110).max(1)
    } else {
        0
    };
    let border_band_x = if background_threshold == 0 {
        (width / 22).max(2)
    } else {
        0
    };
    let border_band_y = if background_threshold == 0 {
        (height / 32).max(2)
    } else {
        0
    };
    // On the 216 mm (638 px @ 75 DPI) flatbed platen, an ISO A4 sheet (210 mm = 620 px @ 75 DPI)
    // ends at width * 620 / 638, casting a vertical paper-edge shadow seam just inside 620/638
    // followed by 6 mm of the white scanner lid. Exclude that right-hand lid overhang in auto mode.
    let a4_limit_x = if background_threshold == 0 && is_bright_background {
        ((width * 620) / 638).max(1)
    } else {
        width
    };
    let max_fg_x = if background_threshold == 0 && is_bright_background {
        ((width * 615) / 638).min(width.saturating_sub(rim_x))
    } else {
        width.saturating_sub(rim_x)
    };

    let mut mask = vec![false; (grid_w * grid_h) as usize];
    for gy in 0..grid_h {
        for gx in 0..grid_w {
            let src_x = (gx * scale).min(width - 1);
            let src_y = (gy * scale).min(height - 1);
            if src_x < rim_x || src_y < rim_y || src_x >= max_fg_x || src_y + rim_y >= height {
                continue;
            }
            let in_border_band = src_x < border_band_x
                || src_y < border_band_y
                || src_x + border_band_x >= width
                || src_y + border_band_y >= height;
            let p = img.get_pixel(src_x, src_y);
            let luma =
                (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as u8;
            let max_c = p[0].max(p[1]).max(p[2]);
            let min_c = p[0].min(p[1]).min(p[2]);
            let chroma = max_c.saturating_sub(min_c);

            let is_foreground = if is_bright_background {
                if in_border_band && background_threshold == 0 {
                    luma < border_bright_thresh || chroma > 20
                } else {
                    luma < effective_bright_thresh || (background_threshold == 0 && chroma > 18)
                }
            } else {
                luma > threshold
            };
            mask[(gy * grid_w + gx) as usize] = is_foreground;
        }
    }

    // Connected Component Labeling via Breadth-First Search
    let mut visited = vec![false; (grid_w * grid_h) as usize];
    let mut bboxes = Vec::new();

    for gy in 0..grid_h {
        for gx in 0..grid_w {
            let start_idx = (gy * grid_w + gx) as usize;
            if !mask[start_idx] || visited[start_idx] {
                continue;
            }

            // Start new component flood fill
            let mut min_x = gx;
            let mut max_x = gx;
            let mut min_y = gy;
            let mut max_y = gy;
            let mut pixel_count = 0u32;

            let mut queue = std::collections::VecDeque::new();
            queue.push_back((gx, gy));
            visited[start_idx] = true;

            while let Some((cx, cy)) = queue.pop_front() {
                pixel_count += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                for (dx, dy) in &[
                    (-1, 0),
                    (1, 0),
                    (0, -1),
                    (0, 1),
                    (-1, -1),
                    (-1, 1),
                    (1, -1),
                    (1, 1),
                ] {
                    let nx = cx as i32 + dx;
                    let ny = cy as i32 + dy;
                    if nx >= 0 && nx < grid_w as i32 && ny >= 0 && ny < grid_h as i32 {
                        let n_idx = (ny as u32 * grid_w + nx as u32) as usize;
                        if mask[n_idx] && !visited[n_idx] {
                            visited[n_idx] = true;
                            queue.push_back((nx as u32, ny as u32));
                        }
                    }
                }
            }

            // Scale bounding box back to original coordinates with small padding
            let pad = 4 * scale;
            let orig_min_x = (min_x * scale).saturating_sub(pad);
            let orig_min_y = (min_y * scale).saturating_sub(pad);
            let orig_max_x = ((max_x + 1) * scale + pad).min(a4_limit_x);
            let orig_max_y = ((max_y + 1) * scale + pad).min(height);

            let box_w = orig_max_x.saturating_sub(orig_min_x);
            let box_h = orig_max_y.saturating_sub(orig_min_y);
            let box_area = box_w * box_h;

            // Filter out dust speckles, thin bezel shadow strips, or full-platen boxes
            let platen_area = width * height;
            let is_thin_border_strip = background_threshold == 0
                && ((box_w < (width / 20).max(4) && box_h > box_w * 4)
                    || (box_h < (height / 20).max(4) && box_w > box_h * 4));
            if box_area >= min_area_px
                && !is_thin_border_strip
                && (box_area as f64) < (platen_area as f64 * 0.95)
                && pixel_count >= 8
            {
                bboxes.push((orig_min_x, orig_min_y, box_w, box_h));
            }
        }
    }

    // Merge overlapping or nested bounding boxes
    let merged_boxes = merge_overlapping_boxes(&bboxes);

    if merged_boxes.is_empty() {
        // Fallback: return A4-trimmed image (excluding 6mm right lid overhang when applicable)
        let out_w = a4_limit_x.min(width);
        let mut sub = ImageBuffer::new(out_w, height);
        for cy in 0..height {
            for cx in 0..out_w {
                sub.put_pixel(cx, cy, *img.get_pixel(cx, cy));
            }
        }
        let geom = ScanGeometry {
            x_offset: 0,
            y_offset: 0,
            width: out_w,
            height,
        };
        return vec![(geom, sub)];
    }

    // Crop sub-images
    let mut results = Vec::with_capacity(merged_boxes.len());
    for (bx, by, bw, bh) in merged_boxes {
        let mut sub = ImageBuffer::new(bw, bh);
        for cy in 0..bh {
            for cx in 0..bw {
                let px = img.get_pixel(bx + cx, by + cy);
                sub.put_pixel(cx, cy, *px);
            }
        }
        let geom = ScanGeometry {
            x_offset: bx,
            y_offset: by,
            width: bw,
            height: bh,
        };
        results.push((geom, sub));
    }

    results
}

/// Estimates document skew angle in degrees using Horizontal Projection Profile variance.
///
/// Skewed text rows blur together vertically, reducing projection variance.
/// The angle that maximizes the variance of horizontal projection bins corresponds
/// to the document skew angle.
pub fn estimate_skew_angle_deg(img: &RgbImage) -> f32 {
    let width = img.width();
    let height = img.height();

    if width < 16 || height < 16 {
        return 0.0;
    }

    // Downsample for fast projection calculation
    let max_dim = width.max(height);
    let scale = (max_dim / 300).max(1);
    let thumb_w = width / scale;
    let thumb_h = height / scale;

    // Extract dark/edge points
    let mut points = Vec::new();
    for ty in 1..thumb_h - 1 {
        for tx in 1..thumb_w - 1 {
            let px = img.get_pixel(tx * scale, ty * scale);
            let luma = 0.299 * px[0] as f32 + 0.587 * px[1] as f32 + 0.114 * px[2] as f32;
            if luma < 160.0 {
                points.push((tx as f32, ty as f32));
            }
        }
    }

    if points.len() < 20 {
        return 0.0;
    }

    let bin_count = thumb_h as usize;

    let calc_variance = |angle_rad: f32| -> f64 {
        let sin_a = angle_rad.sin();
        let cos_a = angle_rad.cos();
        let mut bins = vec![0u32; bin_count];

        for &(x, y) in &points {
            let proj_y = -x * sin_a + y * cos_a;
            let bin = proj_y.round() as isize;
            if bin >= 0 && (bin as usize) < bin_count {
                bins[bin as usize] += 1;
            }
        }

        let n = bin_count as f64;
        let sum: u32 = bins.iter().sum();
        let mean = sum as f64 / n;
        let mut var_sum = 0.0f64;
        for &b in &bins {
            let diff = b as f64 - mean;
            var_sum += diff * diff;
        }
        var_sum / n
    };

    // Coarse search: -12.0 deg to +12.0 deg in 0.5 deg steps
    let mut best_angle = 0.0f32;
    let mut max_variance = -1.0f64;

    let mut deg = -12.0f32;
    while deg <= 12.0 {
        let rad = deg.to_radians();
        let v = calc_variance(rad);
        if v > max_variance {
            max_variance = v;
            best_angle = deg;
        }
        deg += 0.5;
    }

    // Fine search: +/- 0.6 deg around best coarse angle in 0.1 deg steps
    let mut fine_best = best_angle;
    let mut fine_max_v = max_variance;
    let fine_start = best_angle - 0.6;
    let fine_end = best_angle + 0.6;
    let mut fine_deg = fine_start;

    while fine_deg <= fine_end {
        let rad = fine_deg.to_radians();
        let v = calc_variance(rad);
        if v > fine_max_v {
            fine_max_v = v;
            fine_best = fine_deg;
        }
        fine_deg += 0.1;
    }

    fine_best
}

/// Rotates the image by `-skew_angle_deg` using bilinear interpolation to align it horizontally.
pub fn deskew_image(img: &RgbImage) -> RgbImage {
    let angle_deg = estimate_skew_angle_deg(img);
    if angle_deg.abs() < 0.15 {
        return img.clone();
    }

    rotate_bilinear(img, -angle_deg)
}

/// Rotates an image around its center by the specified angle in degrees using bilinear interpolation.
pub fn rotate_bilinear(img: &RgbImage, angle_deg: f32) -> RgbImage {
    let width = img.width();
    let height = img.height();

    if width == 0 || height == 0 || angle_deg.abs() < 0.01 {
        return img.clone();
    }

    let rad = angle_deg.to_radians();
    let cos_a = rad.cos();
    let sin_a = rad.sin();

    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;

    let mut output = ImageBuffer::new(width, height);

    for y in 0..height {
        let dy = y as f32 - cy;
        for x in 0..width {
            let dx = x as f32 - cx;

            // Inverse rotation to map destination to source coordinates
            let src_x = cx + dx * cos_a + dy * sin_a;
            let src_y = cy - dx * sin_a + dy * cos_a;

            if src_x >= 0.0
                && src_x < (width - 1) as f32
                && src_y >= 0.0
                && src_y < (height - 1) as f32
            {
                let x0 = src_x.floor() as u32;
                let y0 = src_y.floor() as u32;
                let x1 = x0 + 1;
                let y1 = y0 + 1;

                let fx = src_x - x0 as f32;
                let fy = src_y - y0 as f32;

                let p00 = img.get_pixel(x0, y0);
                let p10 = img.get_pixel(x1, y0);
                let p01 = img.get_pixel(x0, y1);
                let p11 = img.get_pixel(x1, y1);

                let mut rgb = [0u8; 3];
                for c in 0..3 {
                    let v0 = p00[c] as f32 * (1.0 - fx) + p10[c] as f32 * fx;
                    let v1 = p01[c] as f32 * (1.0 - fx) + p11[c] as f32 * fx;
                    let v = v0 * (1.0 - fy) + v1 * fy;
                    rgb[c] = v.clamp(0.0, 255.0).round() as u8;
                }
                output.put_pixel(x, y, Rgb(rgb));
            } else {
                // Background fill: white for flatbed document borders
                output.put_pixel(x, y, Rgb([255, 255, 255]));
            }
        }
    }

    output
}

/// Computes Otsu's optimal binarization threshold on the image luminance histogram.
fn compute_otsu_threshold(img: &RgbImage) -> u8 {
    let mut hist = [0u32; 256];
    let mut total = 0u32;

    for p in img.pixels() {
        let luma =
            (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as usize;
        hist[luma.min(255)] += 1;
        total += 1;
    }

    if total == 0 {
        return 128;
    }

    let mut sum_all = 0.0f64;
    for (i, &count) in hist.iter().enumerate() {
        sum_all += i as f64 * count as f64;
    }

    let mut sum_bg = 0.0f64;
    let mut weight_bg = 0.0f64;
    let mut max_var = 0.0f64;
    let mut best_thresh = 128u8;

    let total_f = total as f64;

    for (t, &count) in hist.iter().enumerate() {
        weight_bg += count as f64;
        if weight_bg == 0.0 {
            continue;
        }

        let weight_fg = total_f - weight_bg;
        if weight_fg == 0.0 {
            break;
        }

        sum_bg += t as f64 * count as f64;
        let mean_bg = sum_bg / weight_bg;
        let mean_fg = (sum_all - sum_bg) / weight_fg;

        let var_between = weight_bg * weight_fg * (mean_bg - mean_fg).powi(2);
        if var_between > max_var {
            max_var = var_between;
            best_thresh = t as u8;
        }
    }

    best_thresh
}

/// Computes the requested luminance percentile (`0.0..=1.0`) across the image histogram.
fn compute_percentile_luminance(img: &RgbImage, quantile: f32) -> u8 {
    let mut hist = [0u32; 256];
    let mut total = 0u64;
    for p in img.pixels() {
        let luma =
            (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32).round() as usize;
        hist[luma.min(255)] += 1;
        total += 1;
    }
    if total == 0 {
        return 255;
    }
    let target = ((total as f64) * (quantile.clamp(0.0, 1.0) as f64)).round() as u64;
    let mut acc = 0u64;
    for (val, &count) in hist.iter().enumerate() {
        acc += count as u64;
        if acc >= target {
            return val as u8;
        }
    }
    255
}

/// Measures the average luminance at the 4 corners of the platen image.
fn compute_corner_luminance(img: &RgbImage) -> f32 {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 {
        return 255.0;
    }

    let corners = [(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)];

    let mut sum = 0.0f32;
    for &(cx, cy) in &corners {
        let p = img.get_pixel(cx, cy);
        sum += 0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32;
    }
    sum / corners.len() as f32
}

/// Merges overlapping or adjacent bounding boxes until convergence.
fn merge_overlapping_boxes(boxes: &[(u32, u32, u32, u32)]) -> Vec<(u32, u32, u32, u32)> {
    if boxes.is_empty() {
        return Vec::new();
    }

    let mut current: Vec<(u32, u32, u32, u32)> = boxes.to_vec();
    loop {
        let mut merged: Vec<(u32, u32, u32, u32)> = Vec::new();
        let mut any_merged = false;

        for &(bx, by, bw, bh) in &current {
            let mut did_merge = false;
            let b_right = bx + bw;
            let b_bottom = by + bh;

            for m in &mut merged {
                let m_right = m.0 + m.2;
                let m_bottom = m.1 + m.3;

                let overlap_x = bx <= m_right && b_right >= m.0;
                let overlap_y = by <= m_bottom && b_bottom >= m.1;

                if overlap_x && overlap_y {
                    let new_min_x = m.0.min(bx);
                    let new_min_y = m.1.min(by);
                    let new_max_x = m_right.max(b_right);
                    let new_max_y = m_bottom.max(b_bottom);

                    *m = (
                        new_min_x,
                        new_min_y,
                        new_max_x - new_min_x,
                        new_max_y - new_min_y,
                    );
                    did_merge = true;
                    any_merged = true;
                    break;
                }
            }

            if !did_merge {
                merged.push((bx, by, bw, bh));
            }
        }

        current = merged;
        if !any_merged {
            break;
        }
    }

    current.sort_by_key(|a| (a.1, a.0));
    current
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_multi_crop_segments_two_photos() {
        // White flatbed platen 100x60
        let mut img = RgbImage::from_pixel(100, 60, Rgb([255, 255, 255]));

        // Photo 1 at (10, 10) size 30x40 (dark rectangle)
        for y in 10..50 {
            for x in 10..40 {
                img.put_pixel(x, y, Rgb([30, 30, 30]));
            }
        }

        // Photo 2 at (60, 10) size 30x40 (dark rectangle)
        for y in 10..50 {
            for x in 60..90 {
                img.put_pixel(x, y, Rgb([40, 40, 40]));
            }
        }

        let crops = auto_multi_crop(&img, 200, 400);
        assert_eq!(crops.len(), 2);

        let (geom1, sub1) = &crops[0];
        let (geom2, sub2) = &crops[1];

        assert!(geom1.x_offset < 40);
        assert!(geom2.x_offset >= 50);
        assert!(sub1.width() >= 25 && sub1.height() >= 35);
        assert!(sub2.width() >= 25 && sub2.height() >= 35);
    }

    #[test]
    fn deskew_detects_skew_angle_and_rotates() {
        // Create an image with horizontal black text lines
        let mut img = RgbImage::from_pixel(64, 64, Rgb([255, 255, 255]));
        for y in (10..54).step_by(8) {
            for x in 10..54 {
                img.put_pixel(x, y, Rgb([0, 0, 0]));
            }
        }

        // Rotate by +5 degrees
        let rotated = rotate_bilinear(&img, 5.0);
        let angle = estimate_skew_angle_deg(&rotated);

        // Angle detected should be around +5 degrees (+/- 1.5 deg tolerance on small 64x64 test image)
        assert!((angle - 5.0).abs() < 1.5, "Detected angle was {angle}");

        let deskewed = deskew_image(&rotated);
        assert_eq!(deskewed.width(), 64);
        assert_eq!(deskewed.height(), 64);
    }

    #[test]
    fn auto_multi_crop_dark_background() {
        // Open lid with dark platen mat 100x60
        let mut img = RgbImage::from_pixel(100, 60, Rgb([10, 10, 10]));
        // Bright photo on dark mat
        for y in 10..50 {
            for x in 10..50 {
                img.put_pixel(x, y, Rgb([220, 220, 220]));
            }
        }

        let crops = auto_multi_crop(&img, 100, 400);
        assert_eq!(crops.len(), 1);
        assert!(crops[0].1.width() >= 35);
        assert!(crops[0].1.height() >= 35);
    }

    #[test]
    fn auto_multi_crop_uniform_returns_whole_image() {
        let img = RgbImage::from_pixel(50, 50, Rgb([255, 255, 255]));
        let crops = auto_multi_crop(&img, 200, 100);
        assert_eq!(crops.len(), 1);
        assert_eq!(crops[0].0.width, 50);
        assert_eq!(crops[0].0.height, 50);
    }

    #[test]
    fn rotate_bilinear_zero_deg_is_noop() {
        let img = RgbImage::from_pixel(16, 16, Rgb([10, 20, 30]));
        let rotated = rotate_bilinear(&img, 0.0);
        assert_eq!(rotated, img);
    }

    #[test]
    fn auto_multi_crop_ignores_bezel_lid_shadow() {
        // Simulate 300-DPI-like white platen with soft grey bezel shadow on left/right edges
        let mut img = RgbImage::from_pixel(250, 350, Rgb([248, 248, 248]));
        for y in 0..350 {
            for x in 0..8 {
                img.put_pixel(x, y, Rgb([210, 210, 210]));
            }
            for x in 244..250 {
                img.put_pixel(x, y, Rgb([215, 215, 215]));
            }
        }
        // Colored mint tea packet placed near left edge (x=5..150, y=130..320)
        for y in 130..320 {
            for x in 5..150 {
                img.put_pixel(x, y, Rgb([150, 205, 180]));
            }
        }

        let crops = auto_multi_crop(&img, 0, 1000);
        assert_eq!(crops.len(), 1);
        let (geom, sub) = &crops[0];
        assert!(
            geom.y_offset >= 115,
            "Unexpected top margin: {}",
            geom.y_offset
        );
        assert!(sub.width() >= 140 && sub.width() <= 165);
        assert!(sub.height() >= 185 && sub.height() <= 215);
    }
}
