//! Halftoning and multi-level dithering engines for Canon FINE printheads.
//!
//! Provides:
//! - [`DitherAlgorithm`]: Dithering algorithm selector (`BlueNoise`, `Sierra3`, `JarvisJudiceNinke`).
//! - [`BlueNoiseDitherer`]: High-speed parallel threshold matrix dithering using an isotropic
//!   $64 \times 64$ void-and-cluster blue-noise tile across CMYK planes via `rayon`.
//! - [`ErrorDiffusionDitherer`]: Serpentine (alternating left-to-right on even lines,
//!   right-to-left on odd lines) Sierra-3 and Jarvis-Judice-Ninke (JJN) error diffusion.
//! - Multi-level 2-bit quantization matching the 4 dot states of Canon BH-4 / CH-41 heads:
//!   `0` = Off, `1` = Small 2pL droplet, `2` = Medium droplet, `3` = Large 5pL droplet.
//! - 1-bit binary halftoning for draft typography and monochrome passes.

use pixma_color::CmykPixel;
use rayon::prelude::*;
use std::sync::OnceLock;

/// Halftoning / dithering algorithms supported by the raster engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DitherAlgorithm {
    /// Isotropic $64 \times 64$ void-and-cluster blue-noise threshold matrix. High speed via `rayon`.
    BlueNoise,
    /// 3-row Sierra-3 error diffusion with serpentine raster traversal.
    Sierra3,
    /// 3-row Jarvis-Judice-Ninke (JJN) error diffusion with serpentine raster traversal.
    JarvisJudiceNinke,
}

/// Bit depth of the generated halftone dots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DotDepth {
    /// 1-bit binary dots (`0` = Off, `1` = Fired).
    Binary1Bit,
    /// 2-bit multi-level drops (`0` = Off, `1` = Small 2pL, `2` = Medium, `3` = Large 5pL).
    MultiLevel2Bit,
}

// ---------------------------------------------------------------------------
// Blue Noise Matrix & Void-and-Cluster Generator
// ---------------------------------------------------------------------------

const MATRIX_SIZE: usize = 64;
const TOTAL_CELLS: usize = MATRIX_SIZE * MATRIX_SIZE; // 4096

static BLUE_NOISE_MATRIX: OnceLock<Box<[u8; TOTAL_CELLS]>> = OnceLock::new();

/// Returns a reference to the isotropic $64 \times 64$ blue-noise void-and-cluster matrix.
pub fn get_blue_noise_matrix_64x64() -> &'static [u8; TOTAL_CELLS] {
    BLUE_NOISE_MATRIX.get_or_init(|| {
        let matrix = generate_void_and_cluster_64x64();
        Box::new(matrix)
    })
}

/// Generates an isotropic $64 \times 64$ toroidal void-and-cluster blue-noise threshold matrix.
fn generate_void_and_cluster_64x64() -> [u8; TOTAL_CELLS] {
    let mut energy = vec![0.0f32; TOTAL_CELLS];
    let mut matrix = [0u8; TOTAL_CELLS];

    // Precompute toroidal Gaussian distance weights for radius = 8 (sigma = 1.9)
    let sigma = 1.9f32;
    let two_sigma_sq = 2.0 * sigma * sigma;
    let radius = 8i32;

    let mut kernel = Vec::new();
    for dy in -radius..=radius {
        for dx in -radius..=radius {
            let dist_sq = (dx * dx + dy * dy) as f32;
            let w = (-dist_sq / two_sigma_sq).exp();
            if w > 0.001 {
                kernel.push((dx, dy, w));
            }
        }
    }

    // Place initial point at (0, 0)
    let mut taken = vec![false; TOTAL_CELLS];
    let mut next_x = 0;
    let mut next_y = 0;

    for rank in 0..TOTAL_CELLS {
        let idx = next_y * MATRIX_SIZE + next_x;
        taken[idx] = true;
        matrix[idx] = ((rank as u64 * 255) / (TOTAL_CELLS as u64 - 1)) as u8;

        // Add Gaussian energy around chosen point (with toroidal wrap)
        for &(kdx, kdy, w) in &kernel {
            let nx = ((next_x as i32 + kdx).rem_euclid(MATRIX_SIZE as i32)) as usize;
            let ny = ((next_y as i32 + kdy).rem_euclid(MATRIX_SIZE as i32)) as usize;
            energy[ny * MATRIX_SIZE + nx] += w;
        }

        if rank + 1 < TOTAL_CELLS {
            // Find the untaken cell with minimum energy (the largest void)
            let mut min_energy = f32::INFINITY;
            let mut best_x = 0;
            let mut best_y = 0;

            for y in 0..MATRIX_SIZE {
                let row_offset = y * MATRIX_SIZE;
                for x in 0..MATRIX_SIZE {
                    let cell_idx = row_offset + x;
                    if !taken[cell_idx] && energy[cell_idx] < min_energy {
                        min_energy = energy[cell_idx];
                        best_x = x;
                        best_y = y;
                    }
                }
            }
            next_x = best_x;
            next_y = best_y;
        }
    }

    matrix
}

// ---------------------------------------------------------------------------
// Blue Noise Ditherer
// ---------------------------------------------------------------------------

/// Parallel blue-noise threshold matrix ditherer using the $64 \times 64$ void-and-cluster tile.
#[derive(Debug, Default, Clone, Copy)]
pub struct BlueNoiseDitherer;

impl BlueNoiseDitherer {
    /// Creates a new blue-noise ditherer.
    pub fn new() -> Self {
        Self
    }

    /// Dithers an 8-bit channel plane into 2-bit multi-level dot states (`0..=3`).
    pub fn dither_plane_2bit(&self, plane: &[u8], width: usize, height: usize) -> Vec<u8> {
        let matrix = get_blue_noise_matrix_64x64();
        let mut out = vec![0u8; width * height];

        out.par_chunks_exact_mut(width)
            .enumerate()
            .for_each(|(y, row)| {
                let matrix_y = (y % MATRIX_SIZE) * MATRIX_SIZE;
                let src_row = &plane[y * width..(y + 1) * width];

                for x in 0..width {
                    let matrix_idx = matrix_y + (x % MATRIX_SIZE);
                    let threshold = matrix[matrix_idx];
                    let val = src_row[x];

                    // Multi-level 2-bit mapping (levels: 0, 85, 170, 255)
                    let k = val / 85; // 0, 1, or 2
                    let rem = val % 85;
                    let frac = rem * 3; // normalized 0..=255
                    row[x] = if frac > threshold { (k + 1).min(3) } else { k };
                }
            });

        out
    }

    /// Dithers an 8-bit channel plane into 1-bit binary dots (`0` or `1`).
    pub fn dither_plane_1bit(&self, plane: &[u8], width: usize, height: usize) -> Vec<u8> {
        let matrix = get_blue_noise_matrix_64x64();
        let mut out = vec![0u8; width * height];

        out.par_chunks_exact_mut(width)
            .enumerate()
            .for_each(|(y, row)| {
                let matrix_y = (y % MATRIX_SIZE) * MATRIX_SIZE;
                let src_row = &plane[y * width..(y + 1) * width];

                for x in 0..width {
                    let matrix_idx = matrix_y + (x % MATRIX_SIZE);
                    let threshold = matrix[matrix_idx];
                    row[x] = if src_row[x] > threshold { 1 } else { 0 };
                }
            });

        out
    }

    /// Dithers an interleaved `CmykPixel` buffer into 4 independent 2-bit CMYK dot planes.
    pub fn dither_cmyk_2bit(
        &self,
        pixels: &[CmykPixel],
        width: usize,
        height: usize,
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, Vec<u8>) {
        let len = width * height;
        let mut c_in = Vec::with_capacity(len);
        let mut m_in = Vec::with_capacity(len);
        let mut y_in = Vec::with_capacity(len);
        let mut k_in = Vec::with_capacity(len);

        for px in pixels {
            c_in.push(px.c);
            m_in.push(px.m);
            y_in.push(px.y);
            k_in.push(px.k_pgbk);
        }

        let (c_out, (m_out, (y_out, k_out))) = rayon::join(
            || self.dither_plane_2bit(&c_in, width, height),
            || {
                rayon::join(
                    || self.dither_plane_2bit(&m_in, width, height),
                    || {
                        rayon::join(
                            || self.dither_plane_2bit(&y_in, width, height),
                            || self.dither_plane_2bit(&k_in, width, height),
                        )
                    },
                )
            },
        );

        (c_out, m_out, y_out, k_out)
    }

    /// Packs 2-bit dot levels into bytes (4 dots per byte, MSB first).
    pub fn pack_2bit_dots(dots: &[u8]) -> Vec<u8> {
        let out_len = dots.len().div_ceil(4);
        let mut out = Vec::with_capacity(out_len);

        for chunk in dots.chunks(4) {
            let mut byte = 0u8;
            for (idx, &dot) in chunk.iter().enumerate() {
                let shift = (3 - idx) * 2;
                byte |= (dot & 0x03) << shift;
            }
            out.push(byte);
        }

        out
    }

    /// Packs 1-bit binary dots into bytes (8 dots per byte, MSB first).
    pub fn pack_1bit_dots(dots: &[u8]) -> Vec<u8> {
        let out_len = dots.len().div_ceil(8);
        let mut out = Vec::with_capacity(out_len);

        for chunk in dots.chunks(8) {
            let mut byte = 0u8;
            for (idx, &dot) in chunk.iter().enumerate() {
                let shift = 7 - idx;
                byte |= (dot & 0x01) << shift;
            }
            out.push(byte);
        }

        out
    }
}

// ---------------------------------------------------------------------------
// Error Diffusion Ditherer (Serpentine Sierra-3 and JJN)
// ---------------------------------------------------------------------------

/// Serpentine error diffusion halftoning engine.
#[derive(Debug, Clone, Copy)]
pub struct ErrorDiffusionDitherer {
    algorithm: DitherAlgorithm,
}

impl ErrorDiffusionDitherer {
    /// Creates an error diffusion ditherer configured for Sierra-3.
    pub fn new_sierra3() -> Self {
        Self {
            algorithm: DitherAlgorithm::Sierra3,
        }
    }

    /// Creates an error diffusion ditherer configured for Jarvis-Judice-Ninke (JJN).
    pub fn new_jjn() -> Self {
        Self {
            algorithm: DitherAlgorithm::JarvisJudiceNinke,
        }
    }

    /// Dithers an 8-bit channel plane into 2-bit multi-level dot states (`0..=3`).
    pub fn dither_plane_2bit(&self, plane: &[u8], width: usize, height: usize) -> Vec<u8> {
        self.dither_plane_internal(plane, width, height, DotDepth::MultiLevel2Bit)
    }

    /// Dithers an 8-bit channel plane into 1-bit binary dots (`0` or `1`).
    pub fn dither_plane_1bit(&self, plane: &[u8], width: usize, height: usize) -> Vec<u8> {
        self.dither_plane_internal(plane, width, height, DotDepth::Binary1Bit)
    }

    fn dither_plane_internal(
        &self,
        plane: &[u8],
        width: usize,
        height: usize,
        depth: DotDepth,
    ) -> Vec<u8> {
        let mut out = vec![0u8; width * height];
        if width == 0 || height == 0 {
            return out;
        }

        let denom: i32 = match self.algorithm {
            DitherAlgorithm::Sierra3 => 32,
            DitherAlgorithm::JarvisJudiceNinke => 48,
            DitherAlgorithm::BlueNoise => 32,
        };

        // Error buffers for current line, line + 1, and line + 2 with 2-pixel margin on both sides
        let buf_w = width + 4;
        let mut err_curr = vec![0i32; buf_w];
        let mut err_next = vec![0i32; buf_w];
        let mut err_next2 = vec![0i32; buf_w];

        for y in 0..height {
            let row_offset = y * width;
            let is_even_row = (y % 2) == 0;

            if is_even_row {
                // Left-to-right traversal
                for x in 0..width {
                    let buf_x = x + 2;
                    let accum = err_curr[buf_x];
                    let err_rounded = if accum >= 0 {
                        (accum + denom / 2) / denom
                    } else {
                        (accum - denom / 2) / denom
                    };
                    let val = (plane[row_offset + x] as i32) + err_rounded;
                    let (dot, target) = quantize_sample(val, depth);
                    out[row_offset + x] = dot;

                    let err = val - target;
                    self.distribute_error_ltr(
                        &mut err_curr,
                        &mut err_next,
                        &mut err_next2,
                        buf_x,
                        err,
                    );
                }
            } else {
                // Right-to-left traversal (serpentine)
                for x in (0..width).rev() {
                    let buf_x = x + 2;
                    let accum = err_curr[buf_x];
                    let err_rounded = if accum >= 0 {
                        (accum + denom / 2) / denom
                    } else {
                        (accum - denom / 2) / denom
                    };
                    let val = (plane[row_offset + x] as i32) + err_rounded;
                    let (dot, target) = quantize_sample(val, depth);
                    out[row_offset + x] = dot;

                    let err = val - target;
                    self.distribute_error_rtl(
                        &mut err_curr,
                        &mut err_next,
                        &mut err_next2,
                        buf_x,
                        err,
                    );
                }
            }

            // Rotate error line buffers
            std::mem::swap(&mut err_curr, &mut err_next);
            std::mem::swap(&mut err_next, &mut err_next2);
            err_next2.fill(0);
        }

        out
    }

    #[inline(always)]
    fn distribute_error_ltr(
        &self,
        curr: &mut [i32],
        next: &mut [i32],
        next2: &mut [i32],
        x: usize,
        err: i32,
    ) {
        match self.algorithm {
            DitherAlgorithm::Sierra3 => {
                // Sierra-3 Left-to-Right weights (sum = 32):
                // Row 0:   *   5   3
                // Row 1: 2 4   5   4   2
                // Row 2: 0 2   3   2   0
                curr[x + 1] += err * 5;
                curr[x + 2] += err * 3;

                next[x - 2] += err * 2;
                next[x - 1] += err * 4;
                next[x] += err * 5;
                next[x + 1] += err * 4;
                next[x + 2] += err * 2;

                next2[x - 1] += err * 2;
                next2[x] += err * 3;
                next2[x + 1] += err * 2;
            }
            DitherAlgorithm::JarvisJudiceNinke => {
                // Jarvis-Judice-Ninke Left-to-Right weights (sum = 48):
                // Row 0:   *   7   5
                // Row 1: 3 5   7   5   3
                // Row 2: 1 3   5   3   1
                curr[x + 1] += err * 7;
                curr[x + 2] += err * 5;

                next[x - 2] += err * 3;
                next[x - 1] += err * 5;
                next[x] += err * 7;
                next[x + 1] += err * 5;
                next[x + 2] += err * 3;

                next2[x - 2] += err;
                next2[x - 1] += err * 3;
                next2[x] += err * 5;
                next2[x + 1] += err * 3;
                next2[x + 2] += err;
            }
            DitherAlgorithm::BlueNoise => {}
        }
    }

    #[inline(always)]
    fn distribute_error_rtl(
        &self,
        curr: &mut [i32],
        next: &mut [i32],
        next2: &mut [i32],
        x: usize,
        err: i32,
    ) {
        match self.algorithm {
            DitherAlgorithm::Sierra3 => {
                // Sierra-3 Right-to-Left (horizontally mirrored):
                curr[x - 1] += err * 5;
                curr[x - 2] += err * 3;

                next[x + 2] += err * 2;
                next[x + 1] += err * 4;
                next[x] += err * 5;
                next[x - 1] += err * 4;
                next[x - 2] += err * 2;

                next2[x + 1] += err * 2;
                next2[x] += err * 3;
                next2[x - 1] += err * 2;
            }
            DitherAlgorithm::JarvisJudiceNinke => {
                // JJN Right-to-Left (horizontally mirrored):
                curr[x - 1] += err * 7;
                curr[x - 2] += err * 5;

                next[x + 2] += err * 3;
                next[x + 1] += err * 5;
                next[x] += err * 7;
                next[x - 1] += err * 5;
                next[x - 2] += err * 3;

                next2[x + 2] += err;
                next2[x + 1] += err * 3;
                next2[x] += err * 5;
                next2[x - 1] += err * 3;
                next2[x - 2] += err;
            }
            DitherAlgorithm::BlueNoise => {}
        }
    }
}

#[inline(always)]
fn quantize_sample(val: i32, depth: DotDepth) -> (u8, i32) {
    match depth {
        DotDepth::Binary1Bit => {
            if val < 128 {
                (0, 0)
            } else {
                (1, 255)
            }
        }
        DotDepth::MultiLevel2Bit => {
            if val < 43 {
                (0, 0)
            } else if val < 128 {
                (1, 85)
            } else if val < 213 {
                (2, 170)
            } else {
                (3, 255)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_blue_noise_matrix_properties() {
        let matrix = get_blue_noise_matrix_64x64();
        assert_eq!(matrix.len(), 4096);

        // Check min and max coverage
        let min_val = *matrix.iter().min().unwrap();
        let max_val = *matrix.iter().max().unwrap();
        assert_eq!(min_val, 0);
        assert_eq!(max_val, 255);
    }

    #[test]
    fn test_blue_noise_ditherer_gradient() {
        let ditherer = BlueNoiseDitherer::new();
        let width = 128;
        let height = 64;
        let plane: Vec<u8> = (0..height)
            .flat_map(|_| (0..width).map(|x| (x * 255 / width) as u8))
            .collect();

        let dithered_2bit = ditherer.dither_plane_2bit(&plane, width, height);
        assert_eq!(dithered_2bit.len(), width * height);

        // Verify bounds: all outputs are in 0..=3
        for &dot in &dithered_2bit {
            assert!(dot <= 3);
        }

        // Leftmost edge (0) should be mostly 0, rightmost (255) should be 3
        assert_eq!(dithered_2bit[0], 0);
        assert_eq!(dithered_2bit[width - 1], 3);
    }

    #[test]
    fn test_sierra3_error_diffusion() {
        let ditherer = ErrorDiffusionDitherer::new_sierra3();
        let width = 64;
        let height = 32;
        let plane = vec![128u8; width * height];

        let dithered = ditherer.dither_plane_2bit(&plane, width, height);
        assert_eq!(dithered.len(), width * height);

        for &dot in &dithered {
            assert!(dot <= 3);
        }

        // Average output level for midtone (128/255 * 3 = 1.505)
        let sum: u64 = dithered.iter().map(|&d| d as u64).sum();
        let avg = (sum as f64) / (dithered.len() as f64);
        assert!((avg - 1.505).abs() < 0.15);
    }

    #[test]
    fn test_jjn_error_diffusion() {
        let ditherer = ErrorDiffusionDitherer::new_jjn();
        let width = 64;
        let height = 32;
        let plane = vec![85u8; width * height];

        let dithered = ditherer.dither_plane_2bit(&plane, width, height);
        assert_eq!(dithered.len(), width * height);

        for &dot in &dithered {
            assert!(dot <= 3);
        }

        // Average output level for level 1 (85/255 * 3 = 1.0)
        let sum: u64 = dithered.iter().map(|&d| d as u64).sum();
        let avg = (sum as f64) / (dithered.len() as f64);
        assert!((avg - 1.0).abs() < 0.15);
    }

    #[test]
    fn test_packing_dots() {
        let dots_2bit = vec![0, 1, 2, 3, 3, 2, 1, 0];
        let packed = BlueNoiseDitherer::pack_2bit_dots(&dots_2bit);
        assert_eq!(packed.len(), 2);
        // (0 << 6) | (1 << 4) | (2 << 2) | 3 = 0x00 | 0x10 | 0x08 | 0x03 = 0x1B
        assert_eq!(packed[0], 0x1B);
        // (3 << 6) | (2 << 4) | (1 << 2) | 0 = 0xC0 | 0x20 | 0x04 | 0x00 = 0xE4
        assert_eq!(packed[1], 0xE4);

        let dots_1bit = vec![1, 0, 1, 0, 1, 1, 0, 0];
        let packed_1bit = BlueNoiseDitherer::pack_1bit_dots(&dots_1bit);
        assert_eq!(packed_1bit.len(), 1);
        assert_eq!(packed_1bit[0], 0b10101100);
    }

    #[test]
    fn test_binary_1bit_dithering() {
        let width = 32;
        let height = 16;
        let plane = vec![128u8; width * height];

        // BlueNoise 1-bit
        let bn = BlueNoiseDitherer::new();
        let bn_1bit = bn.dither_plane_1bit(&plane, width, height);
        assert_eq!(bn_1bit.len(), width * height);
        for &d in &bn_1bit {
            assert!(d <= 1);
        }

        // Sierra-3 1-bit
        let s3 = ErrorDiffusionDitherer::new_sierra3();
        let s3_1bit = s3.dither_plane_1bit(&plane, width, height);
        assert_eq!(s3_1bit.len(), width * height);
        for &d in &s3_1bit {
            assert!(d <= 1);
        }

        // JJN 1-bit
        let jjn = ErrorDiffusionDitherer::new_jjn();
        let jjn_1bit = jjn.dither_plane_1bit(&plane, width, height);
        assert_eq!(jjn_1bit.len(), width * height);
        for &d in &jjn_1bit {
            assert!(d <= 1);
        }
    }

    #[test]
    fn test_cmyk_parallel_dithering() {
        let bn = BlueNoiseDitherer::new();
        let width = 16;
        let height = 8;
        let pixels = vec![
            CmykPixel {
                c: 50,
                m: 100,
                y: 150,
                k_pgbk: 200,
            };
            width * height
        ];

        let (c, m, y, k) = bn.dither_cmyk_2bit(&pixels, width, height);
        assert_eq!(c.len(), width * height);
        assert_eq!(m.len(), width * height);
        assert_eq!(y.len(), width * height);
        assert_eq!(k.len(), width * height);
    }
}
