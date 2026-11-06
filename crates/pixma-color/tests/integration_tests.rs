//! End-to-end color pipeline and ink separation integration tests.

use pixma_color::{rgb_to_sgray_fixed_point, CmykPixel, ColorPipeline, InkSeparator};
use pixma_core::{ColorAdjustment, MediaType};

#[test]
fn test_glossy_media_hardware_safety_interlock_black_is_composite() {
    // Media types with microporous silica coatings where PGBK pigment would smudge/flake off
    let glossy_media_types = [
        MediaType::GlossyPhoto,
        MediaType::PhotoPlusGlossyII,
        MediaType::PhotoProPlatinum,
        MediaType::PhotoProLuster,
        MediaType::PhotoPlusSemiGloss,
    ];

    let pipeline = ColorPipeline::new(ColorAdjustment::default());

    for media in glossy_media_types {
        let separator = InkSeparator::new(media);

        // Hardware test for pure #000000 black
        let (r, g, b) = pipeline.transform_pixel(0, 0, 0);
        let cmyk = separator.separate_pixel(r, g, b);

        assert_eq!(
            cmyk.k_pgbk, 0,
            "CRITICAL HARDWARE FAILURE: Pigment black must be shut off (0) on {media:?}"
        );
        assert_eq!(
            cmyk.c, 255,
            "Composite black must have 100% Cyan on {media:?}"
        );
        assert_eq!(
            cmyk.m, 255,
            "Composite black must have 100% Magenta on {media:?}"
        );
        assert_eq!(
            cmyk.y, 255,
            "Composite black must have 100% Yellow on {media:?}"
        );
    }
}

#[test]
fn test_plain_paper_waterproof_pigment_black_text() {
    let pipeline = ColorPipeline::new(ColorAdjustment::default());
    let separator = InkSeparator::new(MediaType::Plain);

    // Pure #000000 black text on plain paper
    let (r, g, b) = pipeline.transform_pixel(0, 0, 0);
    let cmyk = separator.separate_pixel(r, g, b);

    assert_eq!(
        cmyk.k_pgbk, 255,
        "Plain paper black text must use 100% Pigment Black (255)"
    );
    assert_eq!(
        cmyk.c, 0,
        "Plain paper black text must have 0% Cyan dye halo"
    );
    assert_eq!(
        cmyk.m, 0,
        "Plain paper black text must have 0% Magenta dye halo"
    );
    assert_eq!(
        cmyk.y, 0,
        "Plain paper black text must have 0% Yellow dye halo"
    );
}

#[test]
fn test_full_pipeline_plain_vs_glossy() {
    let pipeline = ColorPipeline::new(ColorAdjustment::default());
    let plain_sep = InkSeparator::new(MediaType::Plain);
    let glossy_sep = InkSeparator::new(MediaType::PhotoPlusGlossyII);

    let mut rgb_scanline = vec![
        0, 0, 0, // Pure black text
        255, 255, 255, // Paper white
        255, 0, 0, // Primary Red
        0, 255, 0, // Primary Green
        0, 0, 255, // Primary Blue
        128, 128, 128, // 50% Neutral Gray
    ];

    pipeline.transform_scanline(&mut rgb_scanline);

    let mut plain_cmyk = vec![CmykPixel::default(); rgb_scanline.len() / 3];
    let mut glossy_cmyk = vec![CmykPixel::default(); rgb_scanline.len() / 3];

    plain_sep.separate_scanline(&rgb_scanline, &mut plain_cmyk);
    glossy_sep.separate_scanline(&rgb_scanline, &mut glossy_cmyk);

    // 1. Pure black pixel (index 0)
    assert_eq!(plain_cmyk[0], CmykPixel::new(0, 0, 0, 255));
    assert_eq!(glossy_cmyk[0], CmykPixel::new(255, 255, 255, 0));

    // 2. Paper white pixel (index 1)
    assert_eq!(plain_cmyk[1], CmykPixel::new(0, 0, 0, 0));
    assert_eq!(glossy_cmyk[1], CmykPixel::new(0, 0, 0, 0));

    // 3. Red pixel (index 2): M + Y
    assert_eq!(plain_cmyk[2].c, 0);
    assert_eq!(plain_cmyk[2].m, 255);
    assert_eq!(plain_cmyk[2].y, 255);
    assert_eq!(plain_cmyk[2].k_pgbk, 0);

    assert_eq!(glossy_cmyk[2].c, 0);
    assert_eq!(glossy_cmyk[2].m, 255);
    assert_eq!(glossy_cmyk[2].y, 255);
    assert_eq!(glossy_cmyk[2].k_pgbk, 0);

    // 4. Neutral Gray (index 5)
    // Plain paper should extract PGBK via GCR
    assert!(plain_cmyk[5].k_pgbk > 50);
    assert!(plain_cmyk[5].c < 50);

    // Glossy media must NEVER use PGBK, using pure CMY dye composite
    assert_eq!(glossy_cmyk[5].k_pgbk, 0);
    assert_eq!(glossy_cmyk[5].c, 127);
    assert_eq!(glossy_cmyk[5].m, 127);
    assert_eq!(glossy_cmyk[5].y, 127);
}

#[test]
fn test_fixed_point_luminance_macro_match() {
    // Testing ((0x84e * R + 0x1bf0 * G + 0x2d2 * B) >> 13)
    let test_triplets = [
        (0, 0, 0),
        (255, 255, 255),
        (100, 150, 200),
        (50, 100, 30),
        (200, 200, 200),
        (255, 0, 0),
        (0, 255, 0),
        (0, 0, 255),
    ];

    for (r, g, b) in test_triplets {
        let expected =
            ((0x84e * (r as u32) + 0x1bf0 * (g as u32) + 0x2d2 * (b as u32)) >> 13).min(255) as u8;
        let actual = rgb_to_sgray_fixed_point(r, g, b);
        assert_eq!(
            actual, expected,
            "rgb_to_sgray_fixed_point({r}, {g}, {b}) mismatch"
        );
    }
}
