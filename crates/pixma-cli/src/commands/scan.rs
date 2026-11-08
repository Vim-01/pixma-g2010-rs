//! Flatbed scanner acquisition, ScanGear Pro pipeline execution, and document export.

use image::RgbImage;
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_daemon::DaemonClient;
use pixma_imageproc::{execute_scan_pipeline, save_jpeg, save_pdf, save_png, save_tiff};
use std::path::Path;

use crate::args::ScanModeArg;

/// Executes flatbed scan acquisition, image restoration, and file export.
#[allow(clippy::too_many_arguments)]
pub async fn execute_scan(
    client: &mut DaemonClient,
    dpi: u32,
    mode_arg: ScanModeArg,
    output_path: &Path,
    width_opt: Option<u32>,
    height_opt: Option<u32>,
    auto_crop: bool,
    deskew: bool,
    descreen: bool,
    unsharp: bool,
    remove_dust: bool,
    gutter_shadow: bool,
    gamma: f32,
    json: bool,
) -> anyhow::Result<()> {
    let resolution = ScanResolution::from_dpi(dpi)
        .map_err(|e| anyhow::anyhow!("Unsupported scanning resolution: {e}"))?;
    let color_mode: ScanColorMode = mode_arg.into();
    let full = ScanGeometry::full_platen(resolution);
    let a4 = ScanGeometry::a4(resolution);
    let target_width = width_opt.unwrap_or(a4.width).min(full.width).max(1);
    let height = height_opt.unwrap_or(full.height).min(full.height).max(1);

    // Hardware CIS sensor acquires full 32-pixel aligned platen scanlines;
    // we then crop horizontally to `target_width` (exact 210.0 mm A4 by default,
    // excluding the 6 mm right-hand scanner lid overhang unless `--width` was set).
    let geometry = ScanGeometry {
        x_offset: 0,
        y_offset: 0,
        width: full.width,
        height,
    };

    let params = ScanParameters {
        resolution,
        color_mode,
        geometry,
        auto_multi_crop: auto_crop,
        deskew,
        descreen_moire: descreen,
        unsharp_mask: unsharp,
        remove_dust_scratches: remove_dust,
        correct_gutter_shadow: gutter_shadow,
        gamma,
    };

    if !json {
        println!(
            "Scanning flatbed at {} DPI ({:?}, {}x{})...",
            resolution.as_dpi(),
            color_mode,
            target_width,
            geometry.height
        );
    }

    // 1. Acquire raw scanner byte chunks over USB / daemon
    let raw_bytes = client.acquire_scan(&params).await?;
    let width = geometry.width;
    let height = geometry.height;

    // 2. Decode raw scan payload into RGB image
    let full_img = match color_mode {
        ScanColorMode::Color24Bit => {
            let expected_len = (width as usize) * (height as usize) * 3;
            let mut data = raw_bytes;
            if data.len() < expected_len {
                data.resize(expected_len, 255);
            }
            data.truncate(expected_len);
            RgbImage::from_raw(width, height, data)
                .ok_or_else(|| anyhow::anyhow!("Failed to construct RGB image from scan buffer"))?
        }
        ScanColorMode::Grayscale8Bit | ScanColorMode::Lineart1Bit => {
            let expected_len = (width as usize) * (height as usize);
            let mut data = raw_bytes;
            if data.len() < expected_len {
                data.resize(expected_len, 255);
            }
            data.truncate(expected_len);
            let mut rgb_data = Vec::with_capacity(expected_len * 3);
            for b in data {
                rgb_data.push(b);
                rgb_data.push(b);
                rgb_data.push(b);
            }
            RgbImage::from_raw(width, height, rgb_data).ok_or_else(|| {
                anyhow::anyhow!("Failed to construct grayscale RGB image from scan buffer")
            })?
        }
    };

    let raw_img = if target_width < full_img.width() {
        let mut cropped = RgbImage::new(target_width, height);
        for y in 0..height {
            for x in 0..target_width {
                cropped.put_pixel(x, y, *full_img.get_pixel(x, y));
            }
        }
        cropped
    } else {
        full_img
    };

    // 3. Execute ScanGear Pro post-processing pipeline
    let processed_pages = execute_scan_pipeline(raw_img, &params)
        .map_err(|e| anyhow::anyhow!("Scan processing pipeline error: {e}"))?;

    if processed_pages.is_empty() {
        return Err(anyhow::anyhow!("Scan pipeline produced zero pages"));
    }

    // 4. Export to destination format based on file extension
    let ext = output_path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "pdf" => {
            save_pdf(&processed_pages, resolution.as_dpi() as u32, output_path)
                .map_err(|e| anyhow::anyhow!("Failed to save PDF: {e}"))?;
        }
        "png" => {
            if processed_pages.len() == 1 {
                save_png(&processed_pages[0], output_path)
                    .map_err(|e| anyhow::anyhow!("Failed to save PNG: {e}"))?;
            } else {
                for (i, page) in processed_pages.iter().enumerate() {
                    let page_path = if i == 0 {
                        output_path.to_path_buf()
                    } else {
                        let stem = output_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("scan");
                        let parent = output_path.parent().unwrap_or_else(|| Path::new(""));
                        parent.join(format!("{}_{}.png", stem, i + 1))
                    };
                    save_png(page, &page_path)
                        .map_err(|e| anyhow::anyhow!("Failed to save PNG page: {e}"))?;
                }
            }
        }
        "jpg" | "jpeg" => {
            if processed_pages.len() == 1 {
                save_jpeg(&processed_pages[0], 90, output_path)
                    .map_err(|e| anyhow::anyhow!("Failed to save JPEG: {e}"))?;
            } else {
                for (i, page) in processed_pages.iter().enumerate() {
                    let page_path = if i == 0 {
                        output_path.to_path_buf()
                    } else {
                        let stem = output_path
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("scan");
                        let parent = output_path.parent().unwrap_or_else(|| Path::new(""));
                        parent.join(format!("{}_{}.jpg", stem, i + 1))
                    };
                    save_jpeg(page, 90, &page_path)
                        .map_err(|e| anyhow::anyhow!("Failed to save JPEG page: {e}"))?;
                }
            }
        }
        "tif" | "tiff" => {
            save_tiff(&processed_pages[0], output_path)
                .map_err(|e| anyhow::anyhow!("Failed to save TIFF: {e}"))?;
        }
        other => {
            return Err(anyhow::anyhow!(
                "Unsupported file format extension '{other}': expected .pdf, .png, .jpg, or .tiff"
            ));
        }
    }

    if json {
        let result = serde_json::json!({
            "status": "success",
            "dpi": resolution.as_dpi(),
            "color_mode": format!("{:?}", color_mode),
            "pages_exported": processed_pages.len(),
            "output_path": output_path.display().to_string(),
        });
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!(
            "Scan complete: {} page(s) exported to {}",
            processed_pages.len(),
            output_path.display()
        );
    }

    Ok(())
}
