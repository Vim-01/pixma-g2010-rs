//! CUPS print option parser converting command-line options and PPD selections
//! into hardware-validated `PrintJobConfig` parameters.

use pixma_core::print_params::{
    BorderlessExtension, DuplexMode, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintJobConfig, PrintQuality, QuietMode,
};

/// Parses CUPS command-line options string (`argv[5]`) and copies count (`argv[4]`)
/// into a validated [`PrintJobConfig`].
///
/// CUPS options are passed as a space-separated list of `key=value` pairs and
/// boolean flags (e.g., `PageSize=A4.bl MediaType=glossygold Resolution=600dpi Collate outputorder=reverse`).
pub fn parse_cups_options(options_str: &str, copies: u16) -> PrintJobConfig {
    let mut config = PrintJobConfig {
        copies: copies.max(1),
        ..Default::default()
    };

    let tokens = tokenize_cups_options(options_str);

    for (key, val_opt) in tokens {
        let key_lower = key.to_ascii_lowercase();
        let val_str = val_opt.as_deref().unwrap_or("");
        let val_lower = val_str.to_ascii_lowercase();

        match key_lower.as_str() {
            // Paper Size & Media
            "pagesize" | "media" | "papersize" => {
                let (size, is_bl) = parse_paper_size_and_borderless(val_str);
                config.paper_size = size;
                if is_bl && config.borderless == BorderlessExtension::Off {
                    config.borderless = BorderlessExtension::Extension2Med;
                }
            }

            // Media Type
            "mediatype" | "cnspecifymediatype" => {
                config.media_type = parse_media_type(val_str);
            }

            // Resolution & Print Quality
            "resolution" | "outputmode" | "printquality" => {
                config.quality = parse_resolution_quality(val_str);
            }
            "quality" => {
                if let Ok(lvl) = val_str.parse::<u8>() {
                    config.quality = match lvl {
                        1 => PrintQuality::Level1Fine,
                        2 => PrintQuality::Level2High,
                        3 => PrintQuality::Level3Standard,
                        4 => PrintQuality::Level4Fast,
                        5 => PrintQuality::Level5Draft,
                        _ => PrintQuality::Level3Standard,
                    };
                }
            }

            // Grayscale / Color Model
            "colormodel" => match val_lower.as_str() {
                "gray" | "black" | "k" | "monochrome" => config.color.grayscale = true,
                "rgb" | "cmyk" | "color" => config.color.grayscale = false,
                _ => {}
            },
            "cngrayscale" | "grayscale" | "monochrome" => {
                if val_opt.is_none()
                    || val_lower == "true"
                    || val_lower == "1"
                    || val_lower == "yes"
                    || val_lower == "on"
                {
                    config.color.grayscale = true;
                } else if val_lower == "false"
                    || val_lower == "0"
                    || val_lower == "no"
                    || val_lower == "off"
                {
                    config.color.grayscale = false;
                }
            }

            // Borderless Extension Amount
            "cnborderlessextension" | "borderlessextension" => {
                config.borderless = match val_lower.as_str() {
                    "off" | "none" | "0" => BorderlessExtension::Off,
                    "ext0" | "min" => BorderlessExtension::Extension0Min,
                    "ext1" | "1" | "low" => BorderlessExtension::Extension1Low,
                    "ext2" | "2" | "med" | "normal" => BorderlessExtension::Extension2Med,
                    "ext3" | "3" | "max" | "high" => BorderlessExtension::Extension3Max,
                    _ => BorderlessExtension::Extension2Med,
                };
            }

            // Ink Drying Time
            "cninkdryingtime" | "dryingtime" => {
                config.drying_time = match val_lower.as_str() {
                    "short" | "0" => InkDryingTime::Short,
                    "normal" | "1" => InkDryingTime::Normal,
                    "long" | "2" => InkDryingTime::Long,
                    "extralong" | "3" => InkDryingTime::ExtraLong,
                    _ => InkDryingTime::Normal,
                };
            }

            // Paper Abrasion Prevention / Platen Gap
            "cnabrasion" | "abrasion" | "papergap" => {
                if val_opt.is_none()
                    || val_lower == "true"
                    || val_lower == "wide"
                    || val_lower == "1"
                    || val_lower == "on"
                {
                    config.paper_gap = PaperGapMode::WidePreventAbrasion;
                } else {
                    config.paper_gap = PaperGapMode::Auto;
                }
            }

            // Quiet Mode
            "cnquietmode" | "quietmode" | "quiet" => {
                if val_opt.is_none()
                    || val_lower == "always"
                    || val_lower == "true"
                    || val_lower == "1"
                    || val_lower == "on"
                {
                    config.quiet_mode = QuietMode::AlwaysOn;
                } else {
                    config.quiet_mode = QuietMode::Off;
                }
            }

            // Duplex Printing
            "duplex" | "sides" => {
                config.duplex = match val_lower.as_str() {
                    "none" | "off" | "one-sided" | "false" => DuplexMode::Off,
                    "duplexnotumble" | "longedge" | "long-edge" | "two-sided-long-edge" => {
                        DuplexMode::LongEdge
                    }
                    "duplextumble" | "shortedge" | "short-edge" | "two-sided-short-edge" => {
                        DuplexMode::ShortEdge
                    }
                    _ => DuplexMode::Off,
                };
            }

            // Manual Color & Tone Controls
            "cncyan" | "cyan" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.cyan_balance = v.clamp(-50, 50);
                }
            }
            "cnmagenta" | "magenta" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.magenta_balance = v.clamp(-50, 50);
                }
            }
            "cnyellow" | "yellow" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.yellow_balance = v.clamp(-50, 50);
                }
            }
            "cnbrightness" | "brightness" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.brightness = v.clamp(-50, 50);
                }
            }
            "cncontrast" | "contrast" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.contrast = v.clamp(-50, 50);
                }
            }
            "cnintensity" | "intensity" => {
                if let Ok(v) = val_str.parse::<i8>() {
                    config.color.intensity = v.clamp(-50, 50);
                }
            }
            "cngamma" | "gamma" => {
                if let Ok(v) = val_str.parse::<f32>() {
                    if v > 0.0 && v <= 5.0 {
                        config.color.gamma = v;
                    }
                }
            }

            // Page Layout & N-up
            "pagelayout" | "layout" => match val_lower.as_str() {
                "fittopage" | "fit-to-page" => config.layout = PageLayout::FitToPage,
                "booklet" => config.layout = PageLayout::Booklet,
                s if s.starts_with("poster=") || s.starts_with("poster") => {
                    let sub = s
                        .strip_prefix("poster=")
                        .or_else(|| s.strip_prefix("poster"))
                        .unwrap_or("2x2");
                    config.layout = parse_poster_layout(sub);
                }
                s if s.starts_with("scaled=") => {
                    if let Ok(pct) = s["scaled=".len()..].parse::<u16>() {
                        config.layout = PageLayout::Scaled {
                            percent: pct.clamp(20, 400),
                        };
                    }
                }
                _ => config.layout = PageLayout::Normal,
            },
            "number-up" | "nup" => {
                if let Ok(n) = val_str.parse::<u8>() {
                    if [2, 4, 6, 9, 16].contains(&n) {
                        config.layout = PageLayout::NUp { pages_per_sheet: n };
                    }
                }
            }
            "fit-to-page" | "fittopage" => {
                if val_opt.is_none() || val_lower == "true" || val_lower == "1" || val_lower == "on"
                {
                    config.layout = PageLayout::FitToPage;
                }
            }
            "poster" => {
                config.layout = parse_poster_layout(val_str);
            }
            "booklet" => {
                if val_opt.is_none() || val_lower == "true" || val_lower == "1" || val_lower == "on"
                {
                    config.layout = PageLayout::Booklet;
                }
            }

            // Collation
            "collate" => {
                if val_opt.is_none() || val_lower == "true" || val_lower == "1" || val_lower == "on"
                {
                    config.collate = true;
                } else if val_lower == "false" || val_lower == "0" || val_lower == "off" {
                    config.collate = false;
                }
            }
            "no-collate" | "nocollate" => {
                config.collate = false;
            }

            // Output Order (reverse / normal)
            "outputorder" => match val_lower.as_str() {
                "reverse" => config.reverse_order = true,
                "normal" => config.reverse_order = false,
                _ => {}
            },
            "reverse" => {
                if val_opt.is_none() || val_lower == "true" || val_lower == "1" || val_lower == "on"
                {
                    config.reverse_order = true;
                }
            }

            // Copies override
            "copies" => {
                if let Ok(c) = val_str.parse::<u16>() {
                    config.copies = c.max(1);
                }
            }

            _ => {}
        }
    }

    config
}

/// Tokenizes a CUPS options string respecting whitespace and optional quotes.
fn tokenize_cups_options(input: &str) -> Vec<(String, Option<String>)> {
    let mut tokens = Vec::new();
    let mut chars = input.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() {
            chars.next();
            continue;
        }

        // Parse key
        let mut key = String::new();
        while let Some(&c) = chars.peek() {
            if c == '=' || c.is_whitespace() {
                break;
            }
            key.push(c);
            chars.next();
        }

        if key.is_empty() {
            continue;
        }

        // Check for '='
        if chars.peek() == Some(&'=') {
            chars.next(); // consume '='
            let mut val = String::new();
            if chars.peek() == Some(&'"') || chars.peek() == Some(&'\'') {
                let quote = chars.next().unwrap();
                while let Some(&c) = chars.peek() {
                    chars.next();
                    if c == quote {
                        break;
                    }
                    val.push(c);
                }
            } else {
                while let Some(&c) = chars.peek() {
                    if c.is_whitespace() {
                        break;
                    }
                    val.push(c);
                    chars.next();
                }
            }
            tokens.push((key, Some(val)));
        } else {
            // Standalone boolean flag
            tokens.push((key, None));
        }
    }

    tokens
}

/// Parses standard PaperSize and detects borderless modifier (`.bl` suffix).
fn parse_paper_size_and_borderless(raw: &str) -> (PaperSize, bool) {
    let lower = raw.trim().to_ascii_lowercase();
    let is_bl = lower.ends_with(".bl")
        || lower.ends_with(".borderless")
        || lower.ends_with("_borderless")
        || lower.starts_with("borderless_");

    let base = if let Some(stripped) = lower.strip_suffix(".bl") {
        stripped
    } else if let Some(stripped) = lower.strip_suffix(".borderless") {
        stripped
    } else if let Some(stripped) = lower.strip_suffix("_borderless") {
        stripped
    } else if let Some(stripped) = lower.strip_prefix("borderless_") {
        stripped
    } else {
        lower.as_str()
    };

    let size = match base {
        "a4" | "iso_a4_210x297mm" => PaperSize::A4,
        "a5" | "iso_a5_148x210mm" => PaperSize::A5,
        "a6" | "iso_a6_105x148mm" => PaperSize::A6,
        "b5" | "jis_b5_182x257mm" => PaperSize::B5,
        "letter" | "na_letter_8.5x11in" => PaperSize::Letter,
        "legal" | "na_legal_8.5x14in" => PaperSize::Legal,
        "executive" | "na_executive_7.25x10.5in" => PaperSize::Executive,
        "4x6" | "photo4x6" | "na_index-4x6_4x6in" | "4x6in" | "oe_photo-4x6" => PaperSize::Photo4x6,
        "5x7" | "photo5x7" | "na_5x7_5x7in" | "5x7in" | "oe_photo-5x7" => PaperSize::Photo5x7,
        "8x10" | "photo8x10" | "na_govt-letter_8x10in" | "8x10in" => PaperSize::Photo8x10,
        "l" | "photol" | "oe_photo-l_3.5x5in" | "3.5x5" => PaperSize::PhotoL,
        "square5x5" | "5x5" | "oe_square-5x5_5x5in" => PaperSize::Square5x5,
        "businesscard" | "oe_business-card_55x91mm" | "card" => PaperSize::BusinessCard,
        "env10" | "envelopecom10" | "com10" | "na_number-10_4.125x9.5in" => {
            PaperSize::EnvelopeCom10
        }
        "envdl" | "envelopedl" | "dl" | "iso_dl_110x220mm" => PaperSize::EnvelopeDL,
        "postcard" | "hagaki" | "jpn_hagaki_100x148mm" => PaperSize::Hagaki,
        _ => PaperSize::A4,
    };

    (size, is_bl)
}

/// Parses MediaType string.
fn parse_media_type(val: &str) -> MediaType {
    match val.trim().to_ascii_lowercase().as_str() {
        "plain" | "stationery" => MediaType::Plain,
        "glossygold" | "plusglossyii" | "photoplusglossyii" | "custom-media-type-canon-6" => {
            MediaType::PhotoPlusGlossyII
        }
        "proplat" | "photoproplatinum" | "custom-media-type-canon-3" => MediaType::PhotoProPlatinum,
        "luster" | "proluster" | "photoproluster" | "custom-media-type-canon-17" => {
            MediaType::PhotoProLuster
        }
        "semigloss" | "photoplussemigloss" | "custom-media-type-canon-9" => {
            MediaType::PhotoPlusSemiGloss
        }
        "glossypaper" | "glossy" | "photographic-glossy" => MediaType::GlossyPhoto,
        "matte" | "mattephoto" | "custom-media-type-canon-11" => MediaType::MattePhoto,
        "highres" | "highresolution" | "custom-media-type-canon-18" => {
            MediaType::HighResolutionPaper
        }
        "envelope" | "custom-media-type-canon-12" => MediaType::Envelope,
        "ijpostcard" | "inkjethagaki" | "custom-media-type-canon-14" => MediaType::InkjetHagaki,
        "postcard" | "hagaki" | "custom-media-type-canon-15" => MediaType::Hagaki,
        "tshirt" => MediaType::TShirtTransfer,
        "cardstock" | "greetingcard" => MediaType::Cardstock,
        _ => MediaType::Plain,
    }
}

/// Parses Resolution or OutputMode to PrintQuality.
fn parse_resolution_quality(val: &str) -> PrintQuality {
    match val.trim().to_ascii_lowercase().as_str() {
        "300dpi" | "300x300dpi" | "fast_draft" | "draft" | "economy" => PrintQuality::Level5Draft,
        "600dpi" | "600x600dpi" | "normal" | "standard" => PrintQuality::Level3Standard,
        "601dpi" | "high" => PrintQuality::Level2High,
        "1200dpi" | "1200x1200dpi" | "4800x1200dpi" | "best" | "fine" | "photo" => {
            PrintQuality::Level1Fine
        }
        _ => PrintQuality::Level3Standard,
    }
}

/// Parses poster rows and columns (e.g. `2x2`, `3x3`, `4x4`).
fn parse_poster_layout(val: &str) -> PageLayout {
    let lower = val.trim().to_ascii_lowercase();
    let parts: Vec<&str> = lower.split('x').collect();
    if parts.len() == 2 {
        if let (Ok(r), Ok(c)) = (parts[0].parse::<u8>(), parts[1].parse::<u8>()) {
            return PageLayout::Poster {
                rows: r.clamp(1, 8),
                cols: c.clamp(1, 8),
            };
        }
    }
    PageLayout::Poster { rows: 2, cols: 2 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::print_params::{
        BorderlessExtension, DuplexMode, InkDryingTime, MediaType, PageLayout, PaperGapMode,
        PaperSize, PrintQuality, QuietMode,
    };

    #[test]
    fn test_parse_standard_cups_options() {
        let opts =
            "PageSize=A4 MediaType=plain Resolution=600dpi copies=2 Collate outputorder=reverse";
        let config = parse_cups_options(opts, 1);

        assert_eq!(config.paper_size, PaperSize::A4);
        assert_eq!(config.media_type, MediaType::Plain);
        assert_eq!(config.quality, PrintQuality::Level3Standard);
        assert_eq!(config.copies, 2);
        assert!(config.collate);
        assert!(config.reverse_order);
        assert_eq!(config.borderless, BorderlessExtension::Off);
    }

    #[test]
    fn test_parse_borderless_bl_suffix() {
        let opts = "PageSize=A4.bl MediaType=glossygold Resolution=1200dpi";
        let config = parse_cups_options(opts, 1);

        assert_eq!(config.paper_size, PaperSize::A4);
        assert_eq!(config.media_type, MediaType::PhotoPlusGlossyII);
        assert_eq!(config.quality, PrintQuality::Level1Fine);
        assert_eq!(config.borderless, BorderlessExtension::Extension2Med);
    }

    #[test]
    fn test_parse_various_sizes_and_borderless() {
        let test_cases = [
            ("PageSize=Letter.bl", PaperSize::Letter, true),
            ("PageSize=4x6.bl", PaperSize::Photo4x6, true),
            ("PageSize=5x7.bl", PaperSize::Photo5x7, true),
            ("PageSize=8x10", PaperSize::Photo8x10, false),
            ("PageSize=l", PaperSize::PhotoL, false),
            ("PageSize=square5x5.bl", PaperSize::Square5x5, true),
            ("PageSize=businesscard", PaperSize::BusinessCard, false),
            ("PageSize=Env10", PaperSize::EnvelopeCom10, false),
            ("PageSize=EnvDL", PaperSize::EnvelopeDL, false),
            ("PageSize=Postcard", PaperSize::Hagaki, false),
        ];

        for (opt_str, expected_size, expected_bl) in test_cases {
            let cfg = parse_cups_options(opt_str, 1);
            assert_eq!(cfg.paper_size, expected_size);
            if expected_bl {
                assert!(cfg.borderless.is_borderless());
            } else {
                assert_eq!(cfg.borderless, BorderlessExtension::Off);
            }
        }
    }

    #[test]
    fn test_parse_media_types() {
        let types = [
            ("MediaType=proplat", MediaType::PhotoProPlatinum),
            ("MediaType=luster", MediaType::PhotoProLuster),
            ("MediaType=semigloss", MediaType::PhotoPlusSemiGloss),
            ("MediaType=glossypaper", MediaType::GlossyPhoto),
            ("MediaType=matte", MediaType::MattePhoto),
            ("MediaType=highres", MediaType::HighResolutionPaper),
            ("MediaType=envelope", MediaType::Envelope),
            ("MediaType=ijpostcard", MediaType::InkjetHagaki),
            ("MediaType=tshirt", MediaType::TShirtTransfer),
            ("MediaType=cardstock", MediaType::Cardstock),
        ];

        for (opt_str, expected_media) in types {
            let cfg = parse_cups_options(opt_str, 1);
            assert_eq!(cfg.media_type, expected_media);
        }
    }

    #[test]
    fn test_parse_resolution_and_quality() {
        assert_eq!(
            parse_cups_options("Resolution=300dpi", 1).quality,
            PrintQuality::Level5Draft
        );
        assert_eq!(
            parse_cups_options("Resolution=600dpi", 1).quality,
            PrintQuality::Level3Standard
        );
        assert_eq!(
            parse_cups_options("Resolution=601dpi", 1).quality,
            PrintQuality::Level2High
        );
        assert_eq!(
            parse_cups_options("Resolution=1200dpi", 1).quality,
            PrintQuality::Level1Fine
        );
        assert_eq!(
            parse_cups_options("quality=1", 1).quality,
            PrintQuality::Level1Fine
        );
        assert_eq!(
            parse_cups_options("quality=2", 1).quality,
            PrintQuality::Level2High
        );
        assert_eq!(
            parse_cups_options("quality=3", 1).quality,
            PrintQuality::Level3Standard
        );
        assert_eq!(
            parse_cups_options("quality=4", 1).quality,
            PrintQuality::Level4Fast
        );
        assert_eq!(
            parse_cups_options("quality=5", 1).quality,
            PrintQuality::Level5Draft
        );
    }

    #[test]
    fn test_parse_color_adjustments_and_grayscale() {
        let opts = "ColorModel=Gray CNCyan=15 CNMagenta=-10 CNYellow=5 CNBrightness=20 CNContrast=-5 CNIntensity=10 CNGamma=1.8";
        let cfg = parse_cups_options(opts, 1);

        assert!(cfg.color.grayscale);
        assert_eq!(cfg.color.cyan_balance, 15);
        assert_eq!(cfg.color.magenta_balance, -10);
        assert_eq!(cfg.color.yellow_balance, 5);
        assert_eq!(cfg.color.brightness, 20);
        assert_eq!(cfg.color.contrast, -5);
        assert_eq!(cfg.color.intensity, 10);
        assert!((cfg.color.gamma - 1.8).abs() < 1e-4);
    }

    #[test]
    fn test_parse_layout_and_number_up() {
        assert_eq!(
            parse_cups_options("number-up=2", 1).layout,
            PageLayout::NUp { pages_per_sheet: 2 }
        );
        assert_eq!(
            parse_cups_options("number-up=4", 1).layout,
            PageLayout::NUp { pages_per_sheet: 4 }
        );
        assert_eq!(
            parse_cups_options("number-up=16", 1).layout,
            PageLayout::NUp {
                pages_per_sheet: 16
            }
        );
        assert_eq!(
            parse_cups_options("PageLayout=fit-to-page", 1).layout,
            PageLayout::FitToPage
        );
        assert_eq!(
            parse_cups_options("poster=2x2", 1).layout,
            PageLayout::Poster { rows: 2, cols: 2 }
        );
        assert_eq!(
            parse_cups_options("PageLayout=booklet", 1).layout,
            PageLayout::Booklet
        );
    }

    #[test]
    fn test_parse_hardware_modes() {
        let opts = "CNBorderlessExtension=Ext3 CNInkDryingTime=ExtraLong CNAbrasion=True CNQuietMode=Always Duplex=DuplexNoTumble";
        let cfg = parse_cups_options(opts, 1);

        assert_eq!(cfg.borderless, BorderlessExtension::Extension3Max);
        assert_eq!(cfg.drying_time, InkDryingTime::ExtraLong);
        assert_eq!(cfg.paper_gap, PaperGapMode::WidePreventAbrasion);
        assert_eq!(cfg.quiet_mode, QuietMode::AlwaysOn);
        assert_eq!(cfg.duplex, DuplexMode::LongEdge);
    }
}
