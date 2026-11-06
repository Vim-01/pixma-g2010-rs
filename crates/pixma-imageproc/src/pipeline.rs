//! Complete post-scan processing pipeline orchestration.
//!
//! Applies CIS stagger alignment, noise & defect restoration filters, parametric tone
//! curves, deskew, auto-multi-crop, and color mode downsampling according to `ScanParameters`.

use image::{Rgb, RgbImage};
use pixma_core::scan_params::{ScanColorMode, ScanParameters};

use crate::cis_align::LineStaggerBuffer;
use crate::errors::ImageProcError;
use crate::geometry::{auto_multi_crop, deskew_image};
use crate::restoration::{
    correct_gutter_shadow, descreen_moire_2d_fft, remove_dust_and_scratches, unsharp_mask,
};
use crate::tone::ToneCurve;

/// Executes the full post-processing pipeline on an acquired raw scan image.
///
/// Order of operations:
/// 1. CIS physical sensor line-stagger vertical realignment.
/// 2. Morphological dust and scratch removal (if enabled).
/// 3. Book-spine gutter shadow compensation (if enabled).
/// 4. 2D-FFT halftone descreening / moiré suppression (if enabled).
/// 5. Gaussian unsharp masking edge enhancement (if enabled).
/// 6. Gamma / tone curve adjustment (if gamma != 1.0).
/// 7. Automatic document deskew angle correction (if enabled).
/// 8. Multi-photo segmentation / auto-multi-crop (if enabled).
/// 9. Color mode conversion (Color24Bit, Grayscale8Bit, Lineart1Bit).
pub fn execute_scan_pipeline(
    raw_img: RgbImage,
    params: &ScanParameters,
) -> Result<Vec<RgbImage>, ImageProcError> {
    if raw_img.width() == 0 || raw_img.height() == 0 {
        return Err(ImageProcError::EmptyImage);
    }

    // 1. CIS Sensor Line Stagger Realignment
    let mut current = LineStaggerBuffer::align_image(&raw_img, params.resolution);

    // 2. Dust and Scratch Removal
    if params.remove_dust_scratches {
        current = remove_dust_and_scratches(&current, 2, 25);
    }

    // 3. Gutter Shadow Compensation
    if params.correct_gutter_shadow {
        let spine_x = current.width() / 2;
        let shadow_width = current.width() / 4;
        current = correct_gutter_shadow(&current, spine_x, shadow_width);
    }

    // 4. Halftone Moire Descreening via 2D FFT
    if params.descreen_moire {
        current = descreen_moire_2d_fft(&current);
    }

    // 5. Unsharp Masking
    if params.unsharp_mask {
        current = unsharp_mask(&current, 1.5, 0.7);
    }

    // 6. Midtone Gamma / Tone Curve Correction
    if (params.gamma - 1.0).abs() > 0.01 && params.gamma > 0.0 {
        let curve = ToneCurve::from_gamma(params.gamma);
        current = curve.apply_to_image(&current);
    }

    // 7. Deskew
    if params.deskew {
        current = deskew_image(&current);
    }

    // 8. Auto Multi-Crop Segmentation
    let mut images = if params.auto_multi_crop {
        let crops = auto_multi_crop(&current, 0, 10_000);
        let extracted: Vec<RgbImage> = crops.into_iter().map(|(_, img)| img).collect();
        if extracted.is_empty() {
            vec![current]
        } else {
            extracted
        }
    } else {
        vec![current]
    };

    // 9. Color Mode Conversion
    for img in &mut images {
        match params.color_mode {
            ScanColorMode::Color24Bit => {
                // Already 24-bit RGB
            }
            ScanColorMode::Grayscale8Bit => {
                for p in img.pixels_mut() {
                    let luma = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
                        .round() as u8;
                    *p = Rgb([luma, luma, luma]);
                }
            }
            ScanColorMode::Lineart1Bit => {
                for p in img.pixels_mut() {
                    let luma = (0.299 * p[0] as f32 + 0.587 * p[1] as f32 + 0.114 * p[2] as f32)
                        .round() as u8;
                    let bit = if luma < 128 { 0 } else { 255 };
                    *p = Rgb([bit, bit, bit]);
                }
            }
        }
    }

    Ok(images)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::scan_params::ScanResolution;

    #[test]
    fn pipeline_runs_end_to_end() {
        let img = RgbImage::from_pixel(64, 64, Rgb([200, 100, 50]));
        let params = ScanParameters {
            resolution: ScanResolution::Dpi300,
            unsharp_mask: true,
            descreen_moire: false,
            gamma: 1.2,
            ..Default::default()
        };

        let results = execute_scan_pipeline(img, &params).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].width(), 64);
        assert_eq!(results[0].height(), 64);
    }

    #[test]
    fn pipeline_converts_to_grayscale() {
        let img = RgbImage::from_pixel(10, 10, Rgb([200, 100, 50]));
        let params = ScanParameters {
            color_mode: ScanColorMode::Grayscale8Bit,
            ..Default::default()
        };

        let results = execute_scan_pipeline(img, &params).unwrap();
        let p = results[0].get_pixel(0, 0);
        assert_eq!(p[0], p[1]);
        assert_eq!(p[1], p[2]);
    }

    #[test]
    fn pipeline_converts_to_lineart() {
        let mut img = RgbImage::new(10, 10);
        for y in 0..10 {
            for x in 0..10 {
                let v = if x < 5 { 50 } else { 220 };
                img.put_pixel(x, y, Rgb([v, v, v]));
            }
        }

        let params = ScanParameters {
            color_mode: ScanColorMode::Lineart1Bit,
            ..Default::default()
        };

        let results = execute_scan_pipeline(img, &params).unwrap();
        assert_eq!(results[0].get_pixel(0, 0), &Rgb([0, 0, 0]));
        assert_eq!(results[0].get_pixel(7, 0), &Rgb([255, 255, 255]));
    }

    #[test]
    fn pipeline_rejects_empty_image() {
        let empty = RgbImage::new(0, 0);
        let params = ScanParameters::default();
        assert!(execute_scan_pipeline(empty, &params).is_err());
    }

    #[test]
    fn pipeline_executes_all_restoration_filters_together() {
        let img = RgbImage::from_pixel(64, 64, Rgb([180, 180, 180]));
        let params = ScanParameters {
            resolution: ScanResolution::Dpi600,
            unsharp_mask: true,
            descreen_moire: true,
            remove_dust_scratches: true,
            correct_gutter_shadow: true,
            deskew: true,
            auto_multi_crop: false,
            gamma: 1.8,
            ..Default::default()
        };

        let results = execute_scan_pipeline(img, &params).unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].width(), 64);
        assert_eq!(results[0].height(), 64);
    }
}
