//! Page layout engine matching Windows Canon MP Driver imposition and overscan modes.
//!
//! Implements:
//! - [`apply_borderless_extension`]: Central expansion by overscan factors (`1.0`, `1.012`, `1.024`, `1.036`) with edge crop.
//! - [`apply_page_layout`]:
//!   - `PageLayout::Normal`: Center or clip source page onto destination geometry.
//!   - `PageLayout::FitToPage`: Bilinear aspect-ratio scaling to sheet printable area.
//!   - `PageLayout::Scaled`: Uniform enlargement/reduction from 20% to 400%.
//!   - `PageLayout::NUp`: Compositing 2, 4, 6, 9, or 16 pages per sheet with clean cell margins.
//!   - `PageLayout::Poster`: Tiling single pages across $R \times C$ sheets with alignment crop marks.
//!   - `PageLayout::Booklet`: 2-up saddle-stitch signature imposition with 4-page signature padding.

use crate::cups_reader::RasterPageInput;
use crate::errors::RasterError;
use pixma_core::print_params::{BorderlessExtension, PageLayout};
use rayon::prelude::*;

/// Scales the input page from its center by `ext.overscan_factor()` and crops back to target dimensions.
///
/// Simulates the Canon borderless expansion mechanism where photographic rasters bleed
/// 1.2% to 3.6% past physical paper borders to ensure edge-to-edge ink coverage without white margins.
pub fn apply_borderless_extension(
    page: &RasterPageInput,
    ext: BorderlessExtension,
) -> RasterPageInput {
    let factor = ext.overscan_factor();
    if factor <= 1.0 {
        return page.clone();
    }

    let width = page.width;
    let height = page.height;
    let cx = width as f32 / 2.0;
    let cy = height as f32 / 2.0;

    let mut out_pixels = vec![0u8; (width as usize) * (height as usize) * 3];

    out_pixels
        .par_chunks_exact_mut((width as usize) * 3)
        .enumerate()
        .for_each(|(y_idx, row)| {
            let y = y_idx as u32;
            let src_y = cy + (y as f32 + 0.5 - cy) / factor - 0.5;
            let src_y_clamped = src_y.clamp(0.0, (height - 1) as f32);

            let y0 = src_y_clamped.floor() as usize;
            let y1 = (y0 + 1).min(height as usize - 1);
            let fy = src_y_clamped - y0 as f32;

            for x in 0..width {
                let src_x = cx + (x as f32 + 0.5 - cx) / factor - 0.5;
                let src_x_clamped = src_x.clamp(0.0, (width - 1) as f32);

                let x0 = src_x_clamped.floor() as usize;
                let x1 = (x0 + 1).min(width as usize - 1);
                let fx = src_x_clamped - x0 as f32;

                let px_idx = (x as usize) * 3;
                let (r, g, b) = sample_bilinear(page, x0, x1, y0, y1, fx, fy);
                row[px_idx] = r;
                row[px_idx + 1] = g;
                row[px_idx + 2] = b;
            }
        });

    RasterPageInput {
        width,
        height,
        dpi: page.dpi,
        rgb_pixels: out_pixels,
    }
}

/// Rotates a raster page 90 degrees counter-clockwise (converts Landscape to narrow-edge Portrait feed).
pub fn rotate_page_90_counter_clockwise(page: &RasterPageInput) -> RasterPageInput {
    let src_w = page.width as usize;
    let src_h = page.height as usize;
    let mut out_pixels = vec![0u8; src_w * src_h * 3];

    for y in 0..src_h {
        let src_row = y * src_w * 3;
        for x in 0..src_w {
            let src_idx = src_row + x * 3;
            // Counter-clockwise: new_x = y, new_y = src_w - 1 - x
            let dst_x = y;
            let dst_y = src_w - 1 - x;
            let dst_idx = (dst_y * src_h + dst_x) * 3;
            out_pixels[dst_idx] = page.rgb_pixels[src_idx];
            out_pixels[dst_idx + 1] = page.rgb_pixels[src_idx + 1];
            out_pixels[dst_idx + 2] = page.rgb_pixels[src_idx + 2];
        }
    }

    RasterPageInput {
        width: page.height,
        height: page.width,
        dpi: page.dpi,
        rgb_pixels: out_pixels,
    }
}

/// Applies the requested Canon MP driver page layout to an input document page sequence.
pub fn apply_page_layout(
    pages: &[RasterPageInput],
    layout: PageLayout,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    if pages.is_empty() {
        return Err(RasterError::NoPages);
    }
    if target_width == 0 || target_height == 0 {
        return Err(RasterError::InvalidDimensions {
            width: target_width,
            height: target_height,
        });
    }

    // Step A: Auto-orientation alignment
    // If the page aspect orientation (Landscape vs Portrait) does not match the hardware paper feed,
    // auto-rotate by 90 degrees counter-clockwise so the content correctly matches the paper.
    let target_is_landscape = target_width > target_height;
    let oriented_pages: Vec<RasterPageInput> = pages
        .iter()
        .map(|p| {
            let page_is_landscape = p.width > p.height;
            if page_is_landscape != target_is_landscape {
                rotate_page_90_counter_clockwise(p)
            } else {
                p.clone()
            }
        })
        .collect();

    match layout {
        PageLayout::Normal => layout_normal(&oriented_pages, target_width, target_height),
        PageLayout::FitToPage => layout_fit_to_page(&oriented_pages, target_width, target_height),
        PageLayout::Scaled { percent } => {
            layout_scaled(&oriented_pages, percent, target_width, target_height)
        }
        PageLayout::NUp { pages_per_sheet } => {
            layout_nup(&oriented_pages, pages_per_sheet, target_width, target_height)
        }
        PageLayout::Poster { rows, cols } => {
            layout_poster(&oriented_pages, rows, cols, target_width, target_height)
        }
        PageLayout::Booklet => layout_booklet(&oriented_pages, target_width, target_height),
    }
}

// ---------------------------------------------------------------------------
// Internal Layout Implementations
// ---------------------------------------------------------------------------

fn layout_normal(
    pages: &[RasterPageInput],
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let mut out = Vec::with_capacity(pages.len());
    for page in pages {
        if page.width == target_w && page.height == target_h {
            out.push(page.clone());
        } else {
            // Check if page represents a full document sheet that should fit the printable area:
            // If the document is larger than printable area, OR if it matches the paper size
            // within standard page margins (scale between 0.70 and 1.35), scale proportionally
            // to fit printable area so headers, footers and thin vector lines are never clipped!
            let scale_w = target_w as f32 / page.width as f32;
            let scale_h = target_h as f32 / page.height as f32;
            let scale = scale_w.min(scale_h);

            if scale < 1.0 || (scale - 1.0).abs() < 0.35 {
                let fit_w = ((page.width as f32 * scale + 0.5) as u32).max(1);
                let fit_h = ((page.height as f32 * scale + 0.5) as u32).max(1);
                let resized = resize_bilinear(page, fit_w, fit_h)?;
                let mut sheet = RasterPageInput::new_blank(target_w, target_h, page.dpi);
                let offset_x = (target_w as i32 - fit_w as i32) / 2;
                let offset_y = (target_h as i32 - fit_h as i32) / 2;
                blit_image(&mut sheet, &resized, offset_x, offset_y);
                out.push(sheet);
            } else {
                let mut sheet = RasterPageInput::new_blank(target_w, target_h, page.dpi);
                let offset_x = (target_w as i32 - page.width as i32) / 2;
                let offset_y = (target_h as i32 - page.height as i32) / 2;
                blit_image(&mut sheet, page, offset_x, offset_y);
                out.push(sheet);
            }
        }
    }
    Ok(out)
}

fn layout_fit_to_page(
    pages: &[RasterPageInput],
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let mut out = Vec::with_capacity(pages.len());
    for page in pages {
        let scale = (target_w as f32 / page.width as f32).min(target_h as f32 / page.height as f32);
        let fit_w = ((page.width as f32 * scale + 0.5) as u32).max(1);
        let fit_h = ((page.height as f32 * scale + 0.5) as u32).max(1);

        let resized = resize_bilinear(page, fit_w, fit_h)?;
        let mut sheet = RasterPageInput::new_blank(target_w, target_h, page.dpi);
        let offset_x = (target_w as i32 - fit_w as i32) / 2;
        let offset_y = (target_h as i32 - fit_h as i32) / 2;
        blit_image(&mut sheet, &resized, offset_x, offset_y);
        out.push(sheet);
    }
    Ok(out)
}

fn layout_scaled(
    pages: &[RasterPageInput],
    percent: u16,
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    if !(20..=400).contains(&percent) {
        return Err(RasterError::InvalidLayout(format!(
            "Scaled percent must be between 20% and 400%, got {}%",
            percent
        )));
    }

    let scale = percent as f32 / 100.0;
    let mut out = Vec::with_capacity(pages.len());

    for page in pages {
        let sc_w = ((page.width as f32 * scale + 0.5) as u32).max(1);
        let sc_h = ((page.height as f32 * scale + 0.5) as u32).max(1);

        let resized = resize_bilinear(page, sc_w, sc_h)?;
        let mut sheet = RasterPageInput::new_blank(target_w, target_h, page.dpi);
        let offset_x = (target_w as i32 - sc_w as i32) / 2;
        let offset_y = (target_h as i32 - sc_h as i32) / 2;
        blit_image(&mut sheet, &resized, offset_x, offset_y);
        out.push(sheet);
    }
    Ok(out)
}

fn layout_nup(
    pages: &[RasterPageInput],
    pages_per_sheet: u8,
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let (cols, rows) = match pages_per_sheet {
        2 => {
            if target_h >= target_w {
                (1, 2)
            } else {
                (2, 1)
            }
        }
        4 => (2, 2),
        6 => {
            if target_h >= target_w {
                (2, 3)
            } else {
                (3, 2)
            }
        }
        9 => (3, 3),
        16 => (4, 4),
        other => {
            return Err(RasterError::InvalidLayout(format!(
                "Unsupported N-up count: {other}. Expected 2, 4, 6, 9, or 16."
            )));
        }
    };

    let cell_w = target_w / (cols as u32);
    let cell_h = target_h / (rows as u32);
    let pad_x = (cell_w / 25).max(4);
    let pad_y = (cell_h / 25).max(4);
    let usable_w = cell_w.saturating_sub(2 * pad_x).max(1);
    let usable_h = cell_h.saturating_sub(2 * pad_y).max(1);

    let mut out = Vec::new();
    let nup = pages_per_sheet as usize;

    for chunk in pages.chunks(nup) {
        let mut sheet = RasterPageInput::new_blank(target_w, target_h, pages[0].dpi);

        for (idx, page) in chunk.iter().enumerate() {
            let col = (idx as u32) % (cols as u32);
            let row = (idx as u32) / (cols as u32);

            let cell_origin_x = col * cell_w + pad_x;
            let cell_origin_y = row * cell_h + pad_y;

            let scale =
                (usable_w as f32 / page.width as f32).min(usable_h as f32 / page.height as f32);
            let fit_w = ((page.width as f32 * scale + 0.5) as u32).max(1);
            let fit_h = ((page.height as f32 * scale + 0.5) as u32).max(1);

            let resized = resize_bilinear(page, fit_w, fit_h)?;
            let offset_x = cell_origin_x as i32 + (usable_w as i32 - fit_w as i32) / 2;
            let offset_y = cell_origin_y as i32 + (usable_h as i32 - fit_h as i32) / 2;

            blit_image(&mut sheet, &resized, offset_x, offset_y);

            // Draw a subtle border around the cell for alignment
            draw_subtle_box(&mut sheet, col * cell_w, row * cell_h, cell_w, cell_h);
        }

        out.push(sheet);
    }

    Ok(out)
}

fn layout_poster(
    pages: &[RasterPageInput],
    rows: u8,
    cols: u8,
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    if rows == 0 || cols == 0 || rows > 10 || cols > 10 {
        return Err(RasterError::InvalidLayout(format!(
            "Invalid poster grid: {rows}x{cols}. Dimensions must be between 1x1 and 10x10."
        )));
    }

    let total_poster_w = (cols as u32) * target_w;
    let total_poster_h = (rows as u32) * target_h;
    let mut out = Vec::with_capacity(pages.len() * (rows as usize) * (cols as usize));

    for page in pages {
        let poster_full = resize_bilinear(page, total_poster_w, total_poster_h)?;

        for r in 0..rows {
            for c in 0..cols {
                let mut tile = RasterPageInput::new_blank(target_w, target_h, page.dpi);
                let src_x = (c as u32) * target_w;
                let src_y = (r as u32) * target_h;

                for y in 0..target_h {
                    for x in 0..target_w {
                        if let Some(px) = poster_full.get_pixel(src_x + x, src_y + y) {
                            tile.set_pixel(x, y, px);
                        }
                    }
                }

                // Draw alignment ticks on adjoining poster borders
                let tick_len = (target_w / 50).max(12);
                if c < cols - 1 {
                    // Right border alignment ticks
                    draw_vertical_ticks(&mut tile, target_w - 1, tick_len);
                }
                if r < rows - 1 {
                    // Bottom border alignment ticks
                    draw_horizontal_ticks(&mut tile, target_h - 1, tick_len);
                }

                out.push(tile);
            }
        }
    }

    Ok(out)
}

fn layout_booklet(
    pages: &[RasterPageInput],
    target_w: u32,
    target_h: u32,
) -> Result<Vec<RasterPageInput>, RasterError> {
    let num_pages = pages.len();
    let padded_len = num_pages.div_ceil(4) * 4;
    let mut padded_pages: Vec<RasterPageInput> = pages.to_vec();
    while padded_pages.len() < padded_len {
        padded_pages.push(RasterPageInput::new_blank(
            pages[0].width,
            pages[0].height,
            pages[0].dpi,
        ));
    }

    let num_sheets = padded_len / 4;
    let mut out = Vec::with_capacity(num_sheets * 2);

    let half_w = target_w / 2;
    let pad = (half_w / 30).max(6);
    let usable_w = half_w.saturating_sub(2 * pad).max(1);
    let usable_h = target_h.saturating_sub(2 * pad).max(1);

    for s in 0..num_sheets {
        // Front side: Left = M - 2s, Right = 2s + 1 (1-indexed)
        let left_idx_front = (padded_len - 2 * s) - 1;
        let right_idx_front = (2 * s + 1) - 1;

        let mut front_sheet = RasterPageInput::new_blank(target_w, target_h, pages[0].dpi);
        place_booklet_page(
            &mut front_sheet,
            &padded_pages[left_idx_front],
            0,
            half_w,
            usable_w,
            usable_h,
            pad,
        )?;
        place_booklet_page(
            &mut front_sheet,
            &padded_pages[right_idx_front],
            half_w,
            half_w,
            usable_w,
            usable_h,
            pad,
        )?;
        draw_center_fold_guide(&mut front_sheet, half_w);
        out.push(front_sheet);

        // Back side: Left = 2s + 2, Right = M - 2s - 1 (1-indexed)
        let left_idx_back = (2 * s + 2) - 1;
        let right_idx_back = (padded_len - 2 * s - 1) - 1;

        let mut back_sheet = RasterPageInput::new_blank(target_w, target_h, pages[0].dpi);
        place_booklet_page(
            &mut back_sheet,
            &padded_pages[left_idx_back],
            0,
            half_w,
            usable_w,
            usable_h,
            pad,
        )?;
        place_booklet_page(
            &mut back_sheet,
            &padded_pages[right_idx_back],
            half_w,
            half_w,
            usable_w,
            usable_h,
            pad,
        )?;
        draw_center_fold_guide(&mut back_sheet, half_w);
        out.push(back_sheet);
    }

    Ok(out)
}

fn place_booklet_page(
    sheet: &mut RasterPageInput,
    page: &RasterPageInput,
    cell_x: u32,
    _cell_w: u32,
    usable_w: u32,
    usable_h: u32,
    pad: u32,
) -> Result<(), RasterError> {
    let scale = (usable_w as f32 / page.width as f32).min(usable_h as f32 / page.height as f32);
    let fit_w = ((page.width as f32 * scale + 0.5) as u32).max(1);
    let fit_h = ((page.height as f32 * scale + 0.5) as u32).max(1);

    let resized = resize_bilinear(page, fit_w, fit_h)?;
    let offset_x = cell_x as i32 + pad as i32 + (usable_w as i32 - fit_w as i32) / 2;
    let offset_y = pad as i32 + (usable_h as i32 - fit_h as i32) / 2;

    blit_image(sheet, &resized, offset_x, offset_y);
    Ok(())
}

fn draw_center_fold_guide(sheet: &mut RasterPageInput, center_x: u32) {
    let dash = (sheet.height / 80).max(6);
    let mut y = 0;
    while y < sheet.height {
        for dy in 0..dash {
            sheet.set_pixel(center_x, y + dy, [210, 210, 210]);
        }
        y += dash * 3;
    }
}

// ---------------------------------------------------------------------------
// Image Transformation Primitives
// ---------------------------------------------------------------------------

/// High-quality parallel bilinear image resizer.
pub fn resize_bilinear(
    page: &RasterPageInput,
    target_width: u32,
    target_height: u32,
) -> Result<RasterPageInput, RasterError> {
    if target_width == 0 || target_height == 0 {
        return Err(RasterError::InvalidDimensions {
            width: target_width,
            height: target_height,
        });
    }
    if page.width == target_width && page.height == target_height {
        return Ok(page.clone());
    }

    let scale_x = page.width as f32 / target_width as f32;
    let scale_y = page.height as f32 / target_height as f32;

    let mut out_pixels = vec![0u8; (target_width as usize) * (target_height as usize) * 3];

    out_pixels
        .par_chunks_exact_mut((target_width as usize) * 3)
        .enumerate()
        .for_each(|(y_idx, row)| {
            let y = y_idx as u32;
            let src_y = (y as f32 + 0.5) * scale_y - 0.5;
            let src_y_clamped = src_y.clamp(0.0, (page.height - 1) as f32);

            let y0 = src_y_clamped.floor() as usize;
            let y1 = (y0 + 1).min(page.height as usize - 1);
            let fy = src_y_clamped - y0 as f32;

            for x in 0..target_width {
                let src_x = (x as f32 + 0.5) * scale_x - 0.5;
                let src_x_clamped = src_x.clamp(0.0, (page.width - 1) as f32);

                let x0 = src_x_clamped.floor() as usize;
                let x1 = (x0 + 1).min(page.width as usize - 1);
                let fx = src_x_clamped - x0 as f32;

                let (r, g, b) = sample_bilinear(page, x0, x1, y0, y1, fx, fy);
                let px_idx = (x as usize) * 3;
                row[px_idx] = r;
                row[px_idx + 1] = g;
                row[px_idx + 2] = b;
            }
        });

    RasterPageInput::new(target_width, target_height, page.dpi, out_pixels)
}

#[inline(always)]
fn sample_bilinear(
    page: &RasterPageInput,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
    fx: f32,
    fy: f32,
) -> (u8, u8, u8) {
    let w = page.width as usize;
    let idx00 = (y0 * w + x0) * 3;
    let idx10 = (y0 * w + x1) * 3;
    let idx01 = (y1 * w + x0) * 3;
    let idx11 = (y1 * w + x1) * 3;

    let p00 = &page.rgb_pixels[idx00..idx00 + 3];
    let p10 = &page.rgb_pixels[idx10..idx10 + 3];
    let p01 = &page.rgb_pixels[idx01..idx01 + 3];
    let p11 = &page.rgb_pixels[idx11..idx11 + 3];

    let w00 = (1.0 - fx) * (1.0 - fy);
    let w10 = fx * (1.0 - fy);
    let w01 = (1.0 - fx) * fy;
    let w11 = fx * fy;

    let r = (w00 * p00[0] as f32
        + w10 * p10[0] as f32
        + w01 * p01[0] as f32
        + w11 * p11[0] as f32
        + 0.5) as u8;
    let g = (w00 * p00[1] as f32
        + w10 * p10[1] as f32
        + w01 * p01[1] as f32
        + w11 * p11[1] as f32
        + 0.5) as u8;
    let b = (w00 * p00[2] as f32
        + w10 * p10[2] as f32
        + w01 * p01[2] as f32
        + w11 * p11[2] as f32
        + 0.5) as u8;

    (r, g, b)
}

/// Blits `src` onto `dest` at offset `(dest_x, dest_y)` with clipping.
fn blit_image(dest: &mut RasterPageInput, src: &RasterPageInput, dest_x: i32, dest_y: i32) {
    let src_w = src.width as i32;
    let src_h = src.height as i32;
    let dest_w = dest.width as i32;
    let dest_h = dest.height as i32;

    let start_x = dest_x.max(0);
    let start_y = dest_y.max(0);
    let end_x = (dest_x + src_w).min(dest_w);
    let end_y = (dest_y + src_h).min(dest_h);

    if start_x >= end_x || start_y >= end_y {
        return;
    }

    for cy in start_y..end_y {
        let sy = (cy - dest_y) as u32;
        for cx in start_x..end_x {
            let sx = (cx - dest_x) as u32;
            if let Some(px) = src.get_pixel(sx, sy) {
                dest.set_pixel(cx as u32, cy as u32, px);
            }
        }
    }
}

fn draw_subtle_box(sheet: &mut RasterPageInput, x: u32, y: u32, w: u32, h: u32) {
    let border_color = [230, 230, 230];
    for cx in x..(x + w).min(sheet.width) {
        sheet.set_pixel(cx, y, border_color);
        sheet.set_pixel(
            cx,
            (y + h.saturating_sub(1)).min(sheet.height - 1),
            border_color,
        );
    }
    for cy in y..(y + h).min(sheet.height) {
        sheet.set_pixel(x, cy, border_color);
        sheet.set_pixel(
            (x + w.saturating_sub(1)).min(sheet.width - 1),
            cy,
            border_color,
        );
    }
}

fn draw_vertical_ticks(tile: &mut RasterPageInput, x: u32, tick_len: u32) {
    let mark_color = [100, 100, 100];
    let top = (tile.height / 10).min(tile.height.saturating_sub(tick_len));
    let bottom = tile.height.saturating_sub(top + tick_len);
    for dy in 0..tick_len {
        tile.set_pixel(x, top + dy, mark_color);
        tile.set_pixel(x, bottom + dy, mark_color);
    }
}

fn draw_horizontal_ticks(tile: &mut RasterPageInput, y: u32, tick_len: u32) {
    let mark_color = [100, 100, 100];
    let left = (tile.width / 10).min(tile.width.saturating_sub(tick_len));
    let right = tile.width.saturating_sub(left + tick_len);
    for dx in 0..tick_len {
        tile.set_pixel(left + dx, y, mark_color);
        tile.set_pixel(right + dx, y, mark_color);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_pattern(w: u32, h: u32) -> RasterPageInput {
        let mut page = RasterPageInput::new_blank(w, h, 300);
        // Paint corners with distinct colors to detect orientation & scaling
        for y in 0..h {
            for x in 0..w {
                let r = (x * 255 / w) as u8;
                let g = (y * 255 / h) as u8;
                page.set_pixel(x, y, [r, g, 128]);
            }
        }
        page
    }

    #[test]
    fn test_borderless_overscan() {
        let page = create_test_pattern(100, 100);
        let extended = apply_borderless_extension(&page, BorderlessExtension::Extension3Max);
        assert_eq!(extended.width, 100);
        assert_eq!(extended.height, 100);

        // Center pixel should be virtually identical
        let orig_center = page.get_pixel(50, 50).unwrap();
        let ext_center = extended.get_pixel(50, 50).unwrap();
        assert_eq!(orig_center, ext_center);

        // Off should return exact clone
        let off = apply_borderless_extension(&page, BorderlessExtension::Off);
        assert_eq!(off, page);
    }

    #[test]
    fn test_layout_fit_to_page() {
        let page = create_test_pattern(100, 50);
        let result = apply_page_layout(&[page], PageLayout::FitToPage, 200, 200).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].width, 200);
        assert_eq!(result[0].height, 200);
    }

    #[test]
    fn test_layout_scaled() {
        let page = create_test_pattern(100, 100);
        let result = apply_page_layout(
            std::slice::from_ref(&page),
            PageLayout::Scaled { percent: 150 },
            200,
            200,
        )
        .unwrap();
        assert_eq!(result.len(), 1);

        // Invalid scaled percent
        let err = apply_page_layout(&[page], PageLayout::Scaled { percent: 10 }, 200, 200);
        assert!(matches!(err, Err(RasterError::InvalidLayout(_))));
    }

    #[test]
    fn test_layout_nup() {
        let pages: Vec<_> = (0..5).map(|_| create_test_pattern(100, 100)).collect();

        // 4-up with 5 pages should yield 2 sheets
        let sheets =
            apply_page_layout(&pages, PageLayout::NUp { pages_per_sheet: 4 }, 400, 400).unwrap();
        assert_eq!(sheets.len(), 2);
        assert_eq!(sheets[0].width, 400);
        assert_eq!(sheets[0].height, 400);

        // 2-up, 6-up, 9-up, 16-up
        for n in [2, 6, 9, 16] {
            let res = apply_page_layout(&pages, PageLayout::NUp { pages_per_sheet: n }, 400, 400)
                .unwrap();
            assert!(!res.is_empty());
        }

        // Invalid N-up
        let err = apply_page_layout(&pages, PageLayout::NUp { pages_per_sheet: 7 }, 400, 400);
        assert!(matches!(err, Err(RasterError::InvalidLayout(_))));
    }

    #[test]
    fn test_layout_normal_centering() {
        let page = create_test_pattern(50, 50);
        let result = apply_page_layout(&[page], PageLayout::Normal, 100, 100).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].width, 100);
        assert_eq!(result[0].height, 100);
        // Border should be white (255, 255, 255)
        assert_eq!(result[0].get_pixel(0, 0), Some([255, 255, 255]));
    }

    #[test]
    fn test_layout_scaled_bounds() {
        let page = create_test_pattern(100, 100);
        // 20% minimum
        assert!(apply_page_layout(
            std::slice::from_ref(&page),
            PageLayout::Scaled { percent: 20 },
            100,
            100
        )
        .is_ok());
        // 400% maximum
        assert!(apply_page_layout(
            std::slice::from_ref(&page),
            PageLayout::Scaled { percent: 400 },
            100,
            100
        )
        .is_ok());
        // 401% out of range
        assert!(matches!(
            apply_page_layout(
                std::slice::from_ref(&page),
                PageLayout::Scaled { percent: 401 },
                100,
                100
            ),
            Err(RasterError::InvalidLayout(_))
        ));
    }

    #[test]
    fn test_layout_empty_pages() {
        let err = apply_page_layout(&[], PageLayout::Normal, 100, 100);
        assert!(matches!(err, Err(RasterError::NoPages)));
    }

    #[test]
    fn test_layout_poster_2x2() {
        let page = create_test_pattern(200, 200);
        let tiles =
            apply_page_layout(&[page], PageLayout::Poster { rows: 2, cols: 2 }, 100, 100).unwrap();
        assert_eq!(tiles.len(), 4);
        for tile in &tiles {
            assert_eq!(tile.width, 100);
            assert_eq!(tile.height, 100);
        }
    }

    #[test]
    fn test_layout_booklet_signature() {
        let pages: Vec<_> = (0..6).map(|_| create_test_pattern(100, 100)).collect();
        // 6 pages should pad to 8 pages -> 2 sheets (4 sides)
        let sides = apply_page_layout(&pages, PageLayout::Booklet, 400, 300).unwrap();
        assert_eq!(sides.len(), 4);
        for side in &sides {
            assert_eq!(side.width, 400);
            assert_eq!(side.height, 300);
        }
    }

    #[test]
    fn test_layout_auto_orientation_landscape_to_portrait() {
        // Landscape input: 120 width x 80 height
        let page = create_test_pattern(120, 80);
        // Portrait feed: 100 width x 200 height
        let result = apply_page_layout(&[page], PageLayout::Normal, 100, 200).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].width, 100);
        assert_eq!(result[0].height, 200);
    }
}
