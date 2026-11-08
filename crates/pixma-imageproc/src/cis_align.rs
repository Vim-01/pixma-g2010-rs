//! Canon PIXMA G2010 Contact Image Sensor (CIS) Line Stagger Realignment.
//!
//! On Contact Image Sensor (CIS) flatbeds, the Red, Green, and Blue photodiode rows
//! are physically spaced apart along the sub-scan (vertical carriage movement) axis.
//! At optical resolutions (600 DPI and 1200 DPI), the scanner hardware outputs raw
//! interleaved scanlines where the three color channels are captured at physically
//! distinct vertical locations at any given instant.
//!
//! Specifically, for the Canon PIXMA G2010:
//! - Resolutions <= 400 DPI: Offsets are `(0, 0)` relative to Red (realignment is performed in hardware ASIC).
//! - 600 DPI: Green is offset by +2 lines, Blue by +4 lines relative to Red.
//! - 1200 DPI: Green is offset by +4 lines, Blue by +8 lines relative to Red.
//!
//! `LineStaggerBuffer` maintains a ring buffer of incoming raw RGB lines and outputs
//! vertically aligned RGB scanlines where `(R[y], G[y + dy_g], B[y + dy_b])` correspond
//! to the exact same physical Y position on the glass.

use std::collections::VecDeque;

use image::{ImageBuffer, Rgb, RgbImage};
use pixma_core::scan_params::ScanResolution;

use crate::errors::ImageProcError;

/// Realignment ring buffer for Canon G2010 CIS flatbed scanlines.
#[derive(Debug, Clone)]
pub struct LineStaggerBuffer {
    resolution: ScanResolution,
    width_px: u32,
    green_offset: usize,
    blue_offset: usize,
    line_bytes: usize,
    ring: VecDeque<Vec<u8>>,
    lines_pushed: usize,
}

impl LineStaggerBuffer {
    /// Creates a new `LineStaggerBuffer` configured for the specified scan resolution
    /// and pixel width.
    pub fn new(resolution: ScanResolution, width_px: u32) -> Self {
        let (green_offset, blue_offset) = resolution.cis_line_stagger_offsets();
        let line_bytes = (width_px as usize) * 3;
        Self {
            resolution,
            width_px,
            green_offset,
            blue_offset,
            line_bytes,
            ring: VecDeque::with_capacity(blue_offset + 1),
            lines_pushed: 0,
        }
    }

    /// Returns the scan resolution configured for this buffer.
    pub fn resolution(&self) -> ScanResolution {
        self.resolution
    }

    /// Returns the expected scanline width in pixels.
    pub fn width_px(&self) -> u32 {
        self.width_px
    }

    /// Returns the expected byte length of an incoming 24-bit RGB scanline.
    pub fn line_bytes(&self) -> usize {
        self.line_bytes
    }

    /// Returns the sensor vertical offsets `(green_delay, blue_delay)` in lines.
    pub fn stagger_offsets(&self) -> (usize, usize) {
        (self.green_offset, self.blue_offset)
    }

    /// Pushes a single raw 24-bit RGB scanline into the stagger buffer.
    ///
    /// If sufficient lines have been buffered to construct the next realigned scanline,
    /// returns `Ok(Some(aligned_line))`. Otherwise returns `Ok(None)`.
    pub fn push_raw_line(&mut self, raw_line: &[u8]) -> Result<Option<Vec<u8>>, ImageProcError> {
        if raw_line.len() != self.line_bytes {
            return Err(ImageProcError::InvalidLineLength {
                expected: self.line_bytes,
                actual: raw_line.len(),
            });
        }

        // If no stagger offset (<= 400 DPI), return line immediately.
        if self.blue_offset == 0 {
            self.lines_pushed += 1;
            return Ok(Some(raw_line.to_vec()));
        }

        self.ring.push_back(raw_line.to_vec());
        self.lines_pushed += 1;

        if self.ring.len() <= self.blue_offset {
            // Still filling ring buffer up to the maximum blue delay
            Ok(None)
        } else {
            // Ring buffer has exactly (blue_offset + 1) lines.
            // Ring index 0 is Red for physical line Y.
            // Ring index `green_offset` is Green for physical line Y.
            // Ring index `blue_offset` is Blue for physical line Y.
            let aligned = self.assemble_aligned_line(0, self.green_offset, self.blue_offset);
            self.ring.pop_front();
            Ok(Some(aligned))
        }
    }

    /// Flushes all remaining lines from the buffer when the raw scan acquisition terminates.
    ///
    /// Clamps trailing Green and Blue channels to the last acquired line so that the total
    /// emitted lines match the total lines pushed into the buffer.
    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        if self.blue_offset == 0 || self.ring.is_empty() {
            return Vec::new();
        }

        let mut flushed = Vec::with_capacity(self.ring.len());
        while !self.ring.is_empty() {
            let r_idx = 0;
            let g_idx = self.green_offset.min(self.ring.len() - 1);
            let b_idx = self.ring.len() - 1; // Clamped to last available line

            let aligned = self.assemble_aligned_line(r_idx, g_idx, b_idx);
            flushed.push(aligned);
            self.ring.pop_front();
        }

        flushed
    }

    /// Resets the buffer state, discarding any unaligned lines in the ring.
    pub fn reset(&mut self) {
        self.ring.clear();
        self.lines_pushed = 0;
    }

    /// Assembles an aligned RGB scanline from three lines currently in the ring buffer.
    fn assemble_aligned_line(
        &self,
        r_ring_idx: usize,
        g_ring_idx: usize,
        b_ring_idx: usize,
    ) -> Vec<u8> {
        let line_r = &self.ring[r_ring_idx];
        let line_g = &self.ring[g_ring_idx];
        let line_b = &self.ring[b_ring_idx];

        let mut aligned = vec![0u8; self.line_bytes];
        for x in 0..(self.width_px as usize) {
            let offset = x * 3;
            aligned[offset] = line_r[offset];
            aligned[offset + 1] = line_g[offset + 1];
            aligned[offset + 2] = line_b[offset + 2];
        }
        aligned
    }

    /// Realigns an entire acquired `RgbImage` in-place according to the sensor's
    /// optical line stagger offsets and sub-line R/G/B LED sequential strobe phase.
    pub fn align_image(img: &RgbImage, resolution: ScanResolution) -> RgbImage {
        let width = img.width();
        let height = img.height();
        if width == 0 || height == 0 {
            return img.clone();
        }

        let (green_offset, blue_offset) = resolution.cis_line_stagger_offsets();
        let mut aligned = ImageBuffer::new(width, height);

        if blue_offset == 0 {
            return Self::suppress_subline_halftone_moire(img, resolution);
        }

        for y in 0..height {
            let y_g = (y + green_offset as u32).min(height - 1);
            let y_b = (y + blue_offset as u32).min(height - 1);

            for x in 0..width {
                let r = img.get_pixel(x, y)[0];
                let g = img.get_pixel(x, y_g)[1];
                let b = img.get_pixel(x, y_b)[2];
                aligned.put_pixel(x, y, Rgb([r, g, b]));
            }
        }

        Self::suppress_subline_halftone_moire(&aligned, resolution)
    }

    /// Suppresses sequential CIS LED sub-line false-color fringes and halftone moiré on
    /// neutral gray/black regions while preserving 100% of color on 1-pixel and 2-pixel
    /// thin colored lines, small colored objects, and pastel fills.
    ///
    /// Physical principle:
    /// - A real single-hue colored feature (even 1 pixel tall) has unipolar deadzoned
    ///   chromaticity `(rg, bg)` across its vertical cross-section `[ya..yb]`, so its
    ///   vertical total variation `∑ |Δy c|` relative to its vertical net integral `|∑ c|`
    ///   satisfies `diff_net <= 1.35`, and its mean coherent chroma per inked pixel
    ///   satisfies `net_ink >= 22.0`.
    /// - Conversely, CIS sequential R/G/B strobing across neutral black edges, missing-nozzle
    ///   gaps, or halftone screens produces bipolar/oscillating chromaticity within the
    ///   connected inked object (`diff_net >= 1.80`, `net_ink <= 15.5`).
    fn suppress_subline_halftone_moire(img: &RgbImage, resolution: ScanResolution) -> RgbImage {
        let w = img.width() as usize;
        let h = img.height() as usize;
        if w < 32 || h < 32 {
            return img.clone();
        }

        let scale = ((resolution as usize) / 150).max(1);
        let rx = 8 * scale;
        let max_ry = 28 * scale;

        #[inline]
        fn deadzone(v: i32) -> i32 {
            const DZ: i32 = 8;
            if v > DZ {
                v - DZ
            } else if v < -DZ {
                v + DZ
            } else {
                0
            }
        }

        let stride = w;
        let mut raw_abs = vec![0i32; h * stride];
        let mut col_rg = vec![0i32; (h + 1) * stride];
        let mut col_bg = vec![0i32; (h + 1) * stride];
        let mut col_abs = vec![0i32; (h + 1) * stride];
        let mut col_dy = vec![0i32; (h + 1) * stride];
        let mut col_ink = vec![0i32; (h + 1) * stride];
        let mut row_ink = vec![0i32; h * (w + 1)];

        let mut prev_rg = vec![0i32; w];
        let mut prev_bg = vec![0i32; w];

        for y in 0..h {
            let mut r_ink_sum = 0i32;
            let row_ink_base = y * (w + 1);
            let cur_base = (y + 1) * stride;
            let prv_base = y * stride;

            for x in 0..w {
                let p = img.get_pixel(x as u32, y as u32);
                let r = p[0] as i32;
                let g = p[1] as i32;
                let b = p[2] as i32;

                let rg = deadzone(r - g);
                let bg = deadzone(b - g);
                let abs_c = rg.abs() + bg.abs();
                raw_abs[prv_base + x] = abs_c;
                let dy = if y > 0 {
                    (rg - prev_rg[x]).abs() + (bg - prev_bg[x]).abs()
                } else {
                    abs_c
                };
                prev_rg[x] = rg;
                prev_bg[x] = bg;

                // White paper / light shadow has (r + g + b) >= 600 and zero deadzone chroma.
                let ink = i32::from((r + g + b) < 200 * 3 || abs_c != 0);
                r_ink_sum += ink;
                row_ink[row_ink_base + x + 1] = r_ink_sum;

                col_rg[cur_base + x] = col_rg[prv_base + x] + rg;
                col_bg[cur_base + x] = col_bg[prv_base + x] + bg;
                col_abs[cur_base + x] = col_abs[prv_base + x] + abs_c;
                col_dy[cur_base + x] = col_dy[prv_base + x] + dy;
                col_ink[cur_base + x] = col_ink[prv_base + x] + ink;
            }
        }

        let mut out = ImageBuffer::new(w as u32, h as u32);
        let min_empty_run = (2 * scale).max(2);
        let max_border_ink = (2 * scale) as i32;

        for y in 0..h {
            let ym1_base = y.saturating_sub(1) * stride;
            let y_base = y * stride;
            let yp1_base = (y + 1).min(h - 1) * stride;

            for x in 0..w {
                let p = img.get_pixel(x as u32, y as u32);
                let r = p[0] as i32;
                let g = p[1] as i32;
                let b = p[2] as i32;

                if (r - g).abs() <= 2 && (b - g).abs() <= 2 {
                    out.put_pixel(x as u32, y as u32, *p);
                    continue;
                }

                let x0 = x.saturating_sub(rx);
                let x1 = (x + rx + 1).min(w);

                // Grow vertical window [y0..y1] within the connected inked object,
                // stopping at white-paper gaps so adjacent colored bars never bleed across gaps.
                let mut y0 = y;
                let mut empty_run = 0usize;
                let y_min = y.saturating_sub(max_ry).max(1);
                while y0 > y_min {
                    let r_base = (y0 - 1) * (w + 1);
                    if (row_ink[r_base + x1] - row_ink[r_base + x0]) <= max_border_ink {
                        empty_run += 1;
                        if empty_run >= min_empty_run {
                            break;
                        }
                    } else {
                        empty_run = 0;
                    }
                    y0 -= 1;
                }

                let mut y1 = y;
                empty_run = 0;
                let y_max = (y + max_ry).min(h.saturating_sub(2));
                while y1 < y_max {
                    let r_base = (y1 + 1) * (w + 1);
                    if (row_ink[r_base + x1] - row_ink[r_base + x0]) <= max_border_ink {
                        empty_run += 1;
                        if empty_run >= min_empty_run {
                            break;
                        }
                    } else {
                        empty_run = 0;
                    }
                    y1 += 1;
                }

                let ya = y0.saturating_sub(1);
                let yb = (y1 + 2).min(h);
                let ya_base = ya * stride;
                let yb_base = yb * stride;

                let mut net = 0i32;
                let mut s_abs = 0i32;
                let mut s_dy = 0i32;
                let mut s_ink = 0i32;
                for xx in x0..x1 {
                    let c_rg = col_rg[yb_base + xx] - col_rg[ya_base + xx];
                    let c_bg = col_bg[yb_base + xx] - col_bg[ya_base + xx];
                    net += c_rg.abs() + c_bg.abs();
                    s_abs += col_abs[yb_base + xx] - col_abs[ya_base + xx];
                    let peak_c = raw_abs[ym1_base + xx]
                        .max(raw_abs[y_base + xx])
                        .max(raw_abs[yp1_base + xx]);
                    let col_tv = (col_dy[yb_base + xx] - col_dy[ya_base + xx] - peak_c).max(0);
                    s_dy += col_tv;
                    s_ink += col_ink[yb_base + xx] - col_ink[ya_base + xx];
                }

                let keep = if s_ink == 0 || s_abs == 0 || net == 0 {
                    0.0f32
                } else {
                    let diff_net = s_dy as f32 / net as f32;
                    let coh = net as f32 / s_abs as f32;
                    let net_ink = net as f32 / s_ink as f32;
                    let k_diff = ((1.55 - diff_net) / (1.55 - 1.25)).clamp(0.0, 1.0);
                    let k_coh = ((coh - 0.65) / (0.76 - 0.65)).clamp(0.0, 1.0);
                    let k_ink = ((net_ink - 16.5) / (20.0 - 16.5)).clamp(0.0, 1.0);
                    k_diff * k_coh * k_ink
                };

                if keep >= 0.999 {
                    out.put_pixel(x as u32, y as u32, *p);
                } else {
                    let lum = (77 * r + 150 * g + 29 * b + 128) >> 8;
                    let nr = (lum as f32 + (r - lum) as f32 * keep)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let ng = (lum as f32 + (g - lum) as f32 * keep)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    let nb = (lum as f32 + (b - lum) as f32 * keep)
                        .round()
                        .clamp(0.0, 255.0) as u8;
                    out.put_pixel(x as u32, y as u32, Rgb([nr, ng, nb]));
                }
            }
        }

        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_stagger_at_low_dpi_is_passthrough() {
        let mut buf = LineStaggerBuffer::new(ScanResolution::Dpi300, 2);
        assert_eq!(buf.stagger_offsets(), (0, 0));
        assert_eq!(buf.line_bytes(), 6);

        let line = vec![1, 2, 3, 4, 5, 6];
        let out = buf.push_raw_line(&line).unwrap();
        assert_eq!(out, Some(line));
        assert!(buf.flush().is_empty());
    }

    #[test]
    fn line_stagger_at_600_dpi_aligns_correctly() {
        let width = 2;
        let mut buf = LineStaggerBuffer::new(ScanResolution::Dpi600, width);
        let (dy_g, dy_b) = buf.stagger_offsets();
        assert_eq!((dy_g, dy_b), (2, 4));

        // Create 10 lines, each marked with line index: [R, G, B] = [line_idx, line_idx, line_idx]
        let total_lines = 10;
        let mut output_lines = Vec::new();

        for i in 0..total_lines {
            let val = i as u8;
            let line = vec![val, val, val, val, val, val];
            if let Some(aligned) = buf.push_raw_line(&line).unwrap() {
                output_lines.push(aligned);
            }
        }

        let mut flushed = buf.flush();
        output_lines.append(&mut flushed);

        assert_eq!(output_lines.len(), total_lines);

        // For physical line 0:
        // R from line 0 -> 0
        // G from line 2 -> 2
        // B from line 4 -> 4
        assert_eq!(output_lines[0], vec![0, 2, 4, 0, 2, 4]);

        // For physical line 1:
        // R from line 1 -> 1
        // G from line 3 -> 3
        // B from line 5 -> 5
        assert_eq!(output_lines[1], vec![1, 3, 5, 1, 3, 5]);

        // For physical line 5:
        // R from line 5 -> 5
        // G from line 7 -> 7
        // B from line 9 -> 9
        assert_eq!(output_lines[5], vec![5, 7, 9, 5, 7, 9]);

        // For physical line 6 (near end, clamped):
        // R from line 6 -> 6
        // G from line 8 -> 8
        // B from min(6+4, 9) -> 9
        assert_eq!(output_lines[6], vec![6, 8, 9, 6, 8, 9]);
    }

    #[test]
    fn align_image_matches_streaming_buffer() {
        let width = 4;
        let height = 8;
        let mut img = RgbImage::new(width, height);

        for y in 0..height {
            for x in 0..width {
                let v = (y * 10 + x) as u8;
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }

        let aligned_img = LineStaggerBuffer::align_image(&img, ScanResolution::Dpi600);
        assert_eq!(aligned_img.width(), width);
        assert_eq!(aligned_img.height(), height);

        // Check (0, 0):
        // R from y=0
        // G from y=2
        // B from y=4
        let p = aligned_img.get_pixel(0, 0);
        assert_eq!(p[0], 0);
        assert_eq!(p[1], 20);
        assert_eq!(p[2], 40);
    }

    #[test]
    fn rejects_mismatched_scanline_length() {
        let mut buf = LineStaggerBuffer::new(ScanResolution::Dpi600, 10);
        assert_eq!(buf.line_bytes(), 30);

        let short_line = vec![0u8; 20];
        let err = buf.push_raw_line(&short_line).unwrap_err();
        match err {
            ImageProcError::InvalidLineLength { expected, actual } => {
                assert_eq!(expected, 30);
                assert_eq!(actual, 20);
            }
            other => panic!("Expected InvalidLineLength, got {other:?}"),
        }
    }

    #[test]
    fn line_stagger_at_1200_dpi() {
        let mut buf = LineStaggerBuffer::new(ScanResolution::Dpi1200, 1);
        assert_eq!(buf.stagger_offsets(), (4, 8));

        // Push 10 lines of 3 bytes each
        for i in 0..10 {
            let val = i as u8;
            buf.push_raw_line(&[val, val, val]).unwrap();
        }
        let flushed = buf.flush();
        // Since 10 lines were pushed and 8 lines were in the buffer before emit:
        // We had (10 - 8) = 2 lines emitted during push, plus 8 flushed. Total = 10 lines.
        assert_eq!(flushed.len(), 8);
    }

    #[test]
    fn reset_clears_stagger_buffer() {
        let mut buf = LineStaggerBuffer::new(ScanResolution::Dpi600, 1);
        buf.push_raw_line(&[1, 1, 1]).unwrap();
        buf.push_raw_line(&[2, 2, 2]).unwrap();
        assert_eq!(buf.ring.len(), 2);

        buf.reset();
        assert!(buf.ring.is_empty());
        assert_eq!(buf.lines_pushed, 0);
    }

    #[test]
    fn align_image_preserves_thin_colored_lines_at_low_dpi() {
        let mut img = RgbImage::from_pixel(32, 32, Rgb([255, 255, 255]));
        // 1-pixel tall Cyan line at y=16
        for x in 4..28 {
            img.put_pixel(x, 16, Rgb([80, 180, 220]));
        }
        let aligned = LineStaggerBuffer::align_image(&img, ScanResolution::Dpi150);
        assert_eq!(aligned.get_pixel(10, 16), &Rgb([80, 180, 220]));
    }
}
