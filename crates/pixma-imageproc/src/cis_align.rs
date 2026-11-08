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
            // Pass 1: At <= 400 DPI, the CIS LED bar strobes R at y + 0.0, G at y + 1/3, and B at y + 2/3
            // during continuous stepper motion. Compensate the 1/3 and 2/3 sub-line phase shift.
            let mut stage1 = ImageBuffer::new(width, height);
            for y in 0..height {
                let y_prev = y.saturating_sub(1);
                for x in 0..width {
                    let cur = img.get_pixel(x, y);
                    let prev = img.get_pixel(x, y_prev);
                    let r = cur[0];
                    let g = ((2 * cur[1] as u16 + prev[1] as u16 + 1) / 3) as u8;
                    let b = ((cur[2] as u16 + 2 * prev[2] as u16 + 1) / 3) as u8;
                    stage1.put_pixel(x, y, Rgb([r, g, b]));
                }
            }

            if width < 16 || height < 16 {
                return stage1;
            }

            // Pass 2 & 3: Vertical 3-tap chroma median + CIS halftone strobe chroma demodulator.
            // Because sequential R/G/B LED strobes sample 600-DPI halftones at phase offsets
            // (0, 2pi/3, 4pi/3), neutral halftones alias into false rainbow chroma while their
            // local high-frequency chroma jitter `hf_chroma` exceeds `chroma_mag`.
            let w = width as usize;
            let h = height as usize;
            let stride = w + 1;
            let table_len = (h + 1) * stride;
            let mut sum_rg = vec![0i64; table_len];
            let mut sum_bg = vec![0i64; table_len];
            let mut sum_rb = vec![0i64; table_len];
            let mut sum_hf = vec![0i64; table_len];

            for y in 0..h {
                let y_up = y.saturating_sub(1);
                let mut row_rg = 0i64;
                let mut row_bg = 0i64;
                let mut row_rb = 0i64;
                let mut row_hf = 0i64;

                for x in 0..w {
                    let x_lt = x.saturating_sub(1);
                    let p = img.get_pixel(x as u32, y as u32);
                    let p_up = img.get_pixel(x as u32, y_up as u32);
                    let p_lt = img.get_pixel(x_lt as u32, y as u32);

                    let rg = p[0] as i64 - p[1] as i64;
                    let bg = p[2] as i64 - p[1] as i64;
                    let rb = p[0] as i64 - p[2] as i64;

                    let rg_up = p_up[0] as i64 - p_up[1] as i64;
                    let bg_up = p_up[2] as i64 - p_up[1] as i64;
                    let rg_lt = p_lt[0] as i64 - p_lt[1] as i64;
                    let bg_lt = p_lt[2] as i64 - p_lt[1] as i64;

                    let hf = (rg - rg_up).abs()
                        + (bg - bg_up).abs()
                        + (rg - rg_lt).abs()
                        + (bg - bg_lt).abs();

                    row_rg += rg;
                    row_bg += bg;
                    row_rb += rb;
                    row_hf += hf;

                    let idx = (y + 1) * stride + (x + 1);
                    let up_idx = y * stride + (x + 1);
                    sum_rg[idx] = sum_rg[up_idx] + row_rg;
                    sum_bg[idx] = sum_bg[up_idx] + row_bg;
                    sum_rb[idx] = sum_rb[up_idx] + row_rb;
                    sum_hf[idx] = sum_hf[up_idx] + row_hf;
                }
            }

            let box_sum = |tbl: &[i64], x0: usize, y0: usize, x1: usize, y1: usize| -> f32 {
                (tbl[y1 * stride + x1] - tbl[y0 * stride + x1] - tbl[y1 * stride + x0]
                    + tbl[y0 * stride + x0]) as f32
            };

            let median3 = |a: f32, b: f32, c: f32| -> f32 { a.max(b).min(a.min(b).max(c)) };

            let rad = 8usize;
            for y in 0..h {
                let y_prev = y.saturating_sub(1);
                let y_next = (y + 1).min(h - 1);
                let by0 = y.saturating_sub(rad);
                let by1 = (y + rad + 1).min(h);

                for x in 0..w {
                    let bx0 = x.saturating_sub(rad);
                    let bx1 = (x + rad + 1).min(w);
                    let area = ((bx1 - bx0) * (by1 - by0)) as f32;

                    let m_rg = box_sum(&sum_rg, bx0, by0, bx1, by1) / area;
                    let m_bg = box_sum(&sum_bg, bx0, by0, bx1, by1) / area;
                    let m_rb = box_sum(&sum_rb, bx0, by0, bx1, by1) / area;
                    let m_hf = box_sum(&sum_hf, bx0, by0, bx1, by1) / area;

                    let cmag = m_rg.hypot(m_bg);
                    let ratio = m_hf / (cmag + 1.0);

                    let retain = if m_rb.abs() < 12.0 && m_hf > 48.0 {
                        0.0
                    } else if ratio <= 1.15 {
                        1.0
                    } else if ratio >= 1.55 {
                        0.0
                    } else {
                        1.0 - (ratio - 1.15) / 0.40
                    };

                    let p_prev = stage1.get_pixel(x as u32, y_prev as u32);
                    let p_cur = stage1.get_pixel(x as u32, y as u32);
                    let p_next = stage1.get_pixel(x as u32, y_next as u32);

                    let lum_prev = (p_prev[0] as f32 + p_prev[1] as f32 + p_prev[2] as f32) / 3.0;
                    let lum_cur = (p_cur[0] as f32 + p_cur[1] as f32 + p_cur[2] as f32) / 3.0;
                    let lum_next = (p_next[0] as f32 + p_next[1] as f32 + p_next[2] as f32) / 3.0;

                    let cr = median3(
                        p_prev[0] as f32 - lum_prev,
                        p_cur[0] as f32 - lum_cur,
                        p_next[0] as f32 - lum_next,
                    );
                    let cg = median3(
                        p_prev[1] as f32 - lum_prev,
                        p_cur[1] as f32 - lum_cur,
                        p_next[1] as f32 - lum_next,
                    );
                    let cb = median3(
                        p_prev[2] as f32 - lum_prev,
                        p_cur[2] as f32 - lum_cur,
                        p_next[2] as f32 - lum_next,
                    );

                    let r_out = (lum_cur + retain * cr).clamp(0.0, 255.0).round() as u8;
                    let g_out = (lum_cur + retain * cg).clamp(0.0, 255.0).round() as u8;
                    let b_out = (lum_cur + retain * cb).clamp(0.0, 255.0).round() as u8;
                    aligned.put_pixel(x as u32, y as u32, Rgb([r_out, g_out, b_out]));
                }
            }
            return aligned;
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

        aligned
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
    fn align_image_suppresses_cis_halftone_rainbow_while_preserving_true_color() {
        let width = 32;
        let height = 32;
        let mut mono_halftone = RgbImage::new(width, height);

        // Simulate 150-DPI CIS scan of an 8x8 Bayer halftone where G is depressed on even rows
        // and oscillates vertically at the Nyquist frequency while R and B stay near 215.
        for y in 0..height {
            for x in 0..width {
                let checker = if (x + y) % 2 == 0 { 12i32 } else { -12i32 };
                let r = (216 + checker) as u8;
                let b = (212 - checker) as u8;
                let g = if y % 2 == 0 { 174 } else { 204 };
                mono_halftone.put_pixel(x, y, Rgb([r, g, b]));
            }
        }

        let aligned_mono = LineStaggerBuffer::align_image(&mono_halftone, ScanResolution::Dpi150);
        let p_center = aligned_mono.get_pixel(16, 16);
        let spread = p_center[0]
            .max(p_center[1])
            .max(p_center[2])
            .saturating_sub(p_center[0].min(p_center[1]).min(p_center[2]));
        assert!(
            spread <= 2,
            "Expected neutral gray after CIS halftone demodulation, got {:?} (spread={})",
            p_center,
            spread
        );

        // Verify a genuine Magenta color patch (R=235, G=120, B=160) is preserved
        let color_patch = RgbImage::from_pixel(width, height, Rgb([235, 120, 160]));
        let aligned_color = LineStaggerBuffer::align_image(&color_patch, ScanResolution::Dpi150);
        assert_eq!(aligned_color.get_pixel(16, 16), &Rgb([235, 120, 160]));
    }
}
