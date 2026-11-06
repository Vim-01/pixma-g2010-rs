//! Document and image exporters for multi-page PDF, PNG, JPEG, and TIFF.

use std::io::Cursor;
use std::path::Path;

use image::codecs::jpeg::JpegEncoder;
use image::{ExtendedColorType, ImageFormat, RgbImage};
use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};

use crate::errors::ImageProcError;

/// Exports multiple scanned RGB pages into a standard multi-page PDF document.
///
/// Each page's dimensions are calculated from its pixel dimensions and target `dpi`
/// ($1 \text{ pt} = 1/72 \text{ inch}$). Image streams are compressed using Flate / Deflate.
pub fn export_pdf_multipage(pages: &[RgbImage], dpi: u32) -> Result<Vec<u8>, ImageProcError> {
    if pages.is_empty() {
        return Err(ImageProcError::EmptyImage);
    }
    let effective_dpi = if dpi == 0 { 300 } else { dpi } as f32;

    let mut pdf = Pdf::new();
    let catalog_ref = Ref::new(1);
    let pages_ref = Ref::new(2);

    let num_pages = pages.len();
    let mut page_refs = Vec::with_capacity(num_pages);
    let mut image_refs = Vec::with_capacity(num_pages);
    let mut content_refs = Vec::with_capacity(num_pages);

    let mut next_id = 3i32;
    for _ in 0..num_pages {
        page_refs.push(Ref::new(next_id));
        next_id += 1;
        image_refs.push(Ref::new(next_id));
        next_id += 1;
        content_refs.push(Ref::new(next_id));
        next_id += 1;
    }

    // Set catalog and pages tree
    pdf.catalog(catalog_ref).pages(pages_ref);
    pdf.pages(pages_ref)
        .kids(page_refs.iter().copied())
        .count(num_pages as i32);

    for (i, page_img) in pages.iter().enumerate() {
        let w = page_img.width();
        let h = page_img.height();
        if w == 0 || h == 0 {
            return Err(ImageProcError::EmptyImage);
        }

        let width_pt = (w as f32 / effective_dpi) * 72.0;
        let height_pt = (h as f32 / effective_dpi) * 72.0;

        let page_ref = page_refs[i];
        let image_ref = image_refs[i];
        let content_ref = content_refs[i];
        let image_name = Name(b"Im0");

        // Page dictionary
        let mut page = pdf.page(page_ref);
        page.media_box(Rect::new(0.0, 0.0, width_pt, height_pt));
        page.parent(pages_ref);
        page.contents(content_ref);
        let mut resources = page.resources();
        resources.x_objects().pair(image_name, image_ref);
        resources.finish();
        page.finish();

        // Content stream: scale unit image to page dimensions
        let mut content = Content::new();
        content.save_state();
        content.transform([width_pt, 0.0, 0.0, height_pt, 0.0, 0.0]);
        content.x_object(image_name);
        content.restore_state();
        pdf.stream(content_ref, &content.finish());

        // Compress raw RGB image data with Deflate (PNG flate compression)
        let raw_bytes = page_img.as_raw();
        let compressed = miniz_oxide::deflate::compress_to_vec_zlib(raw_bytes, 6);

        // Image XObject dictionary and stream
        let mut image_xobj = pdf.image_xobject(image_ref, &compressed);
        image_xobj.width(w as i32);
        image_xobj.height(h as i32);
        image_xobj.color_space().device_rgb();
        image_xobj.bits_per_component(8);
        image_xobj.filter(Filter::FlateDecode);
        image_xobj.finish();
    }

    Ok(pdf.finish())
}

/// Encodes an RGB image into a PNG byte buffer.
pub fn export_png(img: &RgbImage) -> Result<Vec<u8>, ImageProcError> {
    if img.width() == 0 || img.height() == 0 {
        return Err(ImageProcError::EmptyImage);
    }
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Png)?;
    Ok(buf)
}

/// Encodes an RGB image into a JPEG byte buffer with specified quality (1 to 100).
pub fn export_jpeg(img: &RgbImage, quality: u8) -> Result<Vec<u8>, ImageProcError> {
    if img.width() == 0 || img.height() == 0 {
        return Err(ImageProcError::EmptyImage);
    }
    let q = quality.clamp(1, 100);
    let mut buf = Vec::new();
    let mut encoder = JpegEncoder::new_with_quality(&mut buf, q);
    encoder.encode(
        img.as_raw(),
        img.width(),
        img.height(),
        ExtendedColorType::Rgb8,
    )?;
    Ok(buf)
}

/// Encodes an RGB image into a TIFF byte buffer.
pub fn export_tiff(img: &RgbImage) -> Result<Vec<u8>, ImageProcError> {
    if img.width() == 0 || img.height() == 0 {
        return Err(ImageProcError::EmptyImage);
    }
    let mut buf = Vec::new();
    img.write_to(&mut Cursor::new(&mut buf), ImageFormat::Tiff)?;
    Ok(buf)
}

/// Saves multiple scanned RGB pages into a PDF file on disk.
pub fn save_pdf(
    pages: &[RgbImage],
    dpi: u32,
    path: impl AsRef<Path>,
) -> Result<(), ImageProcError> {
    let bytes = export_pdf_multipage(pages, dpi)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Saves an RGB image into a PNG file on disk.
pub fn save_png(img: &RgbImage, path: impl AsRef<Path>) -> Result<(), ImageProcError> {
    let bytes = export_png(img)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Saves an RGB image into a JPEG file on disk with the specified quality.
pub fn save_jpeg(
    img: &RgbImage,
    quality: u8,
    path: impl AsRef<Path>,
) -> Result<(), ImageProcError> {
    let bytes = export_jpeg(img, quality)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

/// Saves an RGB image into a TIFF file on disk.
pub fn save_tiff(img: &RgbImage, path: impl AsRef<Path>) -> Result<(), ImageProcError> {
    let bytes = export_tiff(img)?;
    std::fs::write(path, bytes)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::Rgb;

    #[test]
    fn exports_valid_multipage_pdf() {
        let p1 = RgbImage::from_pixel(100, 100, Rgb([255, 0, 0]));
        let p2 = RgbImage::from_pixel(100, 100, Rgb([0, 255, 0]));

        let pdf_bytes = export_pdf_multipage(&[p1, p2], 300).unwrap();
        // PDF magic header
        assert!(pdf_bytes.starts_with(b"%PDF-"));
        // Check for EOF marker
        assert!(pdf_bytes.ends_with(b"%%EOF\n") || pdf_bytes.windows(5).any(|w| w == b"%%EOF"));
    }

    #[test]
    fn exports_png_jpeg_tiff() {
        let img = RgbImage::from_pixel(32, 32, Rgb([128, 64, 32]));

        let png = export_png(&img).unwrap();
        assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"));

        let jpeg = export_jpeg(&img, 85).unwrap();
        assert!(jpeg.starts_with(&[0xFF, 0xD8]));

        let tiff = export_tiff(&img).unwrap();
        // TIFF starts with II (little endian) or MM (big endian)
        assert!(tiff.starts_with(b"II\x2a\x00") || tiff.starts_with(b"MM\x00\x2a"));
    }

    #[test]
    fn rejects_empty_images_for_export() {
        let empty = RgbImage::new(0, 0);
        assert!(export_png(&empty).is_err());
        assert!(export_jpeg(&empty, 80).is_err());
        assert!(export_tiff(&empty).is_err());
        assert!(export_pdf_multipage(&[], 300).is_err());
        assert!(export_pdf_multipage(&[empty], 300).is_err());
    }

    #[test]
    fn saves_all_formats_to_disk() {
        let tmp = std::env::temp_dir();
        let img = RgbImage::from_pixel(16, 16, Rgb([50, 100, 150]));

        let p_png = tmp.join("test_export.png");
        let p_jpg = tmp.join("test_export.jpg");
        let p_tif = tmp.join("test_export.tif");
        let p_pdf = tmp.join("test_export.pdf");

        save_png(&img, &p_png).unwrap();
        save_jpeg(&img, 90, &p_jpg).unwrap();
        save_tiff(&img, &p_tif).unwrap();
        save_pdf(&[img], 300, &p_pdf).unwrap();

        assert!(p_png.exists());
        assert!(p_jpg.exists());
        assert!(p_tif.exists());
        assert!(p_pdf.exists());

        // Clean up
        let _ = std::fs::remove_file(p_png);
        let _ = std::fs::remove_file(p_jpg);
        let _ = std::fs::remove_file(p_tif);
        let _ = std::fs::remove_file(p_pdf);
    }
}
