//! Command-line argument specifications for `pixma-cli`.

use clap::{Parser, Subcommand, ValueEnum};
use pixma_core::maintenance::{CleaningTarget, CleaningType, RollerCleanTarget};
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintQuality, QuietMode,
};
use pixma_core::scan_params::ScanColorMode;
use std::path::PathBuf;

/// Command-line interface for Canon PIXMA G2010 administration, maintenance,
/// scanning, and direct raster printing.
#[derive(Parser, Debug, Clone)]
#[command(
    name = "pixma-cli",
    author = "Vim-01",
    version,
    about = "Administration, maintenance, scanning, and printing utility for Canon PIXMA G2010",
    long_about = None
)]
pub struct Cli {
    /// Path to pixmad Unix domain socket.
    #[arg(long, global = true, help = "Path to pixmad Unix domain socket")]
    pub socket: Option<PathBuf>,

    /// Connect to in-memory virtual G2010 hardware emulator instead of physical USB.
    #[arg(
        long,
        global = true,
        help = "Use in-memory virtual G2010 hardware emulator"
    )]
    pub mock: bool,

    /// Format output as machine-readable JSON.
    #[arg(
        long,
        global = true,
        help = "Emit output in machine-readable JSON format"
    )]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug, Clone, PartialEq)]
pub enum Commands {
    /// Query real-time hardware status, ink levels, and diagnostic error codes.
    Status,

    /// Execute print head cleaning routine.
    Clean {
        /// Cleaning type: regular (standard), deep (heavy), or flush (system choke).
        #[arg(
            long,
            value_enum,
            default_value = "regular",
            help = "Cleaning intensity"
        )]
        kind: CleanKindArg,

        /// Target head / ink group: all, black (PGBK), or color (CMY).
        #[arg(
            long,
            value_enum,
            default_value = "all",
            help = "Cleaning target ink group"
        )]
        target: CleanTargetArg,

        /// Confirmation token required for Ink Flush (must be 'YES-DRAIN-INK').
        #[arg(
            long,
            help = "Mandatory confirmation token for Ink Flush ('YES-DRAIN-INK')"
        )]
        confirm: Option<String>,

        /// Dangerously bypass safety cooldown timers.
        #[arg(
            long = "override-cooldown-dangerously",
            help = "Bypass mandatory cooldown timers"
        )]
        override_cooldown_dangerously: bool,
    },

    /// Clean mechanical paper feed rollers or platen bottom plate ribs.
    RollerClean {
        /// Cleaning target: roller (paper feed roller) or platen (bottom plate).
        #[arg(long, value_enum, default_value = "roller", help = "Mechanical target")]
        target: RollerTargetArg,
    },

    /// Print diagnostic nozzle check calibration grid.
    NozzleCheck,

    /// Print head alignment routines and adjustment offsets.
    Align {
        /// Alignment routine action: print-manual, half-auto, check, or apply.
        #[arg(long, value_enum, help = "Alignment action")]
        action: AlignActionArg,

        /// Comma-separated integer offsets (-5..=5) for columns A..N (required when action=apply).
        #[arg(
            long,
            help = "14 comma-separated integer offsets (-5..=5) for columns A..N"
        )]
        values: Option<String>,
    },

    /// Acquire image from the flatbed scanner platen.
    Scan {
        /// Optical scanning resolution in DPI.
        #[arg(
            long,
            default_value = "300",
            help = "Scanning resolution (75, 100, 150, 200, 300, 400, 600, 1200)"
        )]
        dpi: u32,

        /// Color acquisition mode.
        #[arg(long, value_enum, default_value = "color", help = "Color mode")]
        mode: ScanModeArg,

        /// Output image/document destination path (.pdf, .png, .jpg, .tiff).
        #[arg(long, help = "Output destination file path (.pdf, .png, .jpg, .tiff)")]
        output: PathBuf,

        /// Scan region width in pixels (defaults to platen width).
        #[arg(long, help = "Scan region width in pixels")]
        width: Option<u32>,

        /// Scan region height in pixels (defaults to platen height).
        #[arg(long, help = "Scan region height in pixels")]
        height: Option<u32>,

        /// Automatically detect, crop, and segment individual photos on the platen.
        #[arg(long = "auto-crop", help = "Enable automatic multi-photo crop")]
        auto_crop: bool,

        /// Automatically detect and rectify document rotation skew.
        #[arg(long, help = "Enable automatic document deskew")]
        deskew: bool,

        /// Suppress halftone print dot moire patterns using 2D Fast Fourier Transform.
        #[arg(long, help = "Enable 2D-FFT halftone descreening")]
        descreen: bool,

        /// Enhance edges using Gaussian unsharp masking.
        #[arg(long, help = "Enable Gaussian unsharp masking edge sharpening")]
        unsharp: bool,

        /// Suppress dust specks and scratches via morphological median filtering.
        #[arg(long = "remove-dust", help = "Enable dust and scratch removal")]
        remove_dust: bool,

        /// Correct book-spine shadow darkening near page gutters.
        #[arg(long = "gutter-shadow", help = "Enable gutter shadow compensation")]
        gutter_shadow: bool,

        /// Scanner gamma correction exponent.
        #[arg(long, default_value = "2.2", help = "Midtone gamma adjustment")]
        gamma: f32,
    },

    /// Print an image, PWG Raster document, or internal test page.
    Print {
        /// Input document or image path.
        #[arg(long, help = "Input image or PWG Raster file path")]
        input: Option<PathBuf>,

        /// Print internal diagnostic calibration page (A4 CMYK step bars and comb).
        #[arg(
            long = "test-page",
            help = "Print diagnostic test page",
            conflicts_with = "input"
        )]
        test_page: bool,

        /// Printing media type.
        #[arg(long, value_enum, default_value = "plain", help = "Paper media type")]
        media: MediaTypeArg,

        /// Standard paper dimensions.
        #[arg(long, value_enum, default_value = "a4", help = "Paper size")]
        paper: PaperSizeArg,

        /// Print quality level (1 = Finest to 5 = Fast Draft).
        #[arg(
            long,
            default_value = "3",
            value_parser = clap::value_parser!(u8).range(1..=5),
            help = "Print quality level (1 = Finest, 5 = Draft)"
        )]
        quality: u8,

        /// Borderless overscan expansion amount (off, 0, 1, 2, 3).
        #[arg(
            long,
            value_enum,
            default_value = "off",
            help = "Borderless overscan mode"
        )]
        borderless: BorderlessArg,

        /// Page layout imposition mode.
        #[arg(long, value_enum, default_value = "normal", help = "Page layout mode")]
        layout: LayoutArg,

        /// Convert color graphics to monochrome grayscale.
        #[arg(long, help = "Convert printout to grayscale")]
        grayscale: bool,

        /// Cyan color balance adjustment (-50..=50).
        #[arg(
            long,
            default_value = "0",
            allow_hyphen_values = true,
            value_parser = clap::value_parser!(i8).range(-50..=50),
            help = "Cyan color balance adjustment (-50..50)"
        )]
        cyan: i8,

        /// Magenta color balance adjustment (-50..=50).
        #[arg(
            long,
            default_value = "0",
            allow_hyphen_values = true,
            value_parser = clap::value_parser!(i8).range(-50..=50),
            help = "Magenta color balance adjustment (-50..50)"
        )]
        magenta: i8,

        /// Yellow color balance adjustment (-50..=50).
        #[arg(
            long,
            default_value = "0",
            allow_hyphen_values = true,
            value_parser = clap::value_parser!(i8).range(-50..=50),
            help = "Yellow color balance adjustment (-50..50)"
        )]
        yellow: i8,

        /// Brightness adjustment (-50..=50).
        #[arg(
            long,
            default_value = "0",
            allow_hyphen_values = true,
            value_parser = clap::value_parser!(i8).range(-50..=50),
            help = "Brightness adjustment (-50..50)"
        )]
        brightness: i8,

        /// Contrast adjustment (-50..=50).
        #[arg(
            long,
            default_value = "0",
            allow_hyphen_values = true,
            value_parser = clap::value_parser!(i8).range(-50..=50),
            help = "Contrast adjustment (-50..50)"
        )]
        contrast: i8,

        /// Widen head-to-platen gap to prevent paper abrasion on thick media.
        #[arg(long = "wide-gap", help = "Prevent paper abrasion (wide platen gap)")]
        wide_gap: bool,

        /// Ink drying pause duration between printed sheets.
        #[arg(
            long,
            value_enum,
            default_value = "normal",
            help = "Ink drying wait time"
        )]
        drying: DryingArg,

        /// Acoustic quiet operation mode.
        #[arg(long, help = "Enable acoustic quiet mode")]
        quiet: bool,
    },
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanKindArg {
    Regular,
    Deep,
    Flush,
}

impl From<CleanKindArg> for CleaningType {
    fn from(arg: CleanKindArg) -> Self {
        match arg {
            CleanKindArg::Regular => CleaningType::Regular,
            CleanKindArg::Deep => CleaningType::Deep,
            CleanKindArg::Flush => CleaningType::InkFlush,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum CleanTargetArg {
    All,
    Black,
    Color,
}

impl From<CleanTargetArg> for CleaningTarget {
    fn from(arg: CleanTargetArg) -> Self {
        match arg {
            CleanTargetArg::All => CleaningTarget::All,
            CleanTargetArg::Black => CleaningTarget::BlackOnly,
            CleanTargetArg::Color => CleaningTarget::ColorOnly,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum RollerTargetArg {
    Roller,
    Platen,
}

impl From<RollerTargetArg> for RollerCleanTarget {
    fn from(arg: RollerTargetArg) -> Self {
        match arg {
            RollerTargetArg::Roller => RollerCleanTarget::FeedRoller,
            RollerTargetArg::Platen => RollerCleanTarget::BottomPlate,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignActionArg {
    #[value(name = "print-manual")]
    PrintManual,
    #[value(name = "half-auto")]
    HalfAuto,
    Check,
    Apply,
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanModeArg {
    Color,
    Gray,
    Lineart,
}

impl From<ScanModeArg> for ScanColorMode {
    fn from(arg: ScanModeArg) -> Self {
        match arg {
            ScanModeArg::Color => ScanColorMode::Color24Bit,
            ScanModeArg::Gray => ScanColorMode::Grayscale8Bit,
            ScanModeArg::Lineart => ScanColorMode::Lineart1Bit,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTypeArg {
    Plain,
    Glossygold,
    Proplat,
    Luster,
    Semigloss,
    Glossy,
    Matte,
    Highres,
    Envelope,
    Hagaki,
    Cardstock,
}

impl From<MediaTypeArg> for MediaType {
    fn from(arg: MediaTypeArg) -> Self {
        match arg {
            MediaTypeArg::Plain => MediaType::Plain,
            MediaTypeArg::Glossygold => MediaType::PhotoPlusGlossyII,
            MediaTypeArg::Proplat => MediaType::PhotoProPlatinum,
            MediaTypeArg::Luster => MediaType::PhotoProLuster,
            MediaTypeArg::Semigloss => MediaType::PhotoPlusSemiGloss,
            MediaTypeArg::Glossy => MediaType::GlossyPhoto,
            MediaTypeArg::Matte => MediaType::MattePhoto,
            MediaTypeArg::Highres => MediaType::HighResolutionPaper,
            MediaTypeArg::Envelope => MediaType::Envelope,
            MediaTypeArg::Hagaki => MediaType::Hagaki,
            MediaTypeArg::Cardstock => MediaType::Cardstock,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperSizeArg {
    A4,
    A5,
    B5,
    Letter,
    Legal,
    #[value(name = "4x6")]
    Photo4x6,
    #[value(name = "5x7")]
    Photo5x7,
    #[value(name = "8x10")]
    Photo8x10,
    L,
    #[value(name = "square5x5")]
    Square5x5,
    Businesscard,
    Env10,
    Envdl,
    Hagaki,
}

impl From<PaperSizeArg> for PaperSize {
    fn from(arg: PaperSizeArg) -> Self {
        match arg {
            PaperSizeArg::A4 => PaperSize::A4,
            PaperSizeArg::A5 => PaperSize::A5,
            PaperSizeArg::B5 => PaperSize::B5,
            PaperSizeArg::Letter => PaperSize::Letter,
            PaperSizeArg::Legal => PaperSize::Legal,
            PaperSizeArg::Photo4x6 => PaperSize::Photo4x6,
            PaperSizeArg::Photo5x7 => PaperSize::Photo5x7,
            PaperSizeArg::Photo8x10 => PaperSize::Photo8x10,
            PaperSizeArg::L => PaperSize::PhotoL,
            PaperSizeArg::Square5x5 => PaperSize::Square5x5,
            PaperSizeArg::Businesscard => PaperSize::BusinessCard,
            PaperSizeArg::Env10 => PaperSize::EnvelopeCom10,
            PaperSizeArg::Envdl => PaperSize::EnvelopeDL,
            PaperSizeArg::Hagaki => PaperSize::Hagaki,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BorderlessArg {
    Off,
    #[value(name = "0")]
    Level0,
    #[value(name = "1")]
    Level1,
    #[value(name = "2")]
    Level2,
    #[value(name = "3")]
    Level3,
}

impl From<BorderlessArg> for BorderlessExtension {
    fn from(arg: BorderlessArg) -> Self {
        match arg {
            BorderlessArg::Off => BorderlessExtension::Off,
            BorderlessArg::Level0 => BorderlessExtension::Extension0Min,
            BorderlessArg::Level1 => BorderlessExtension::Extension1Low,
            BorderlessArg::Level2 => BorderlessExtension::Extension2Med,
            BorderlessArg::Level3 => BorderlessExtension::Extension3Max,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum LayoutArg {
    Normal,
    Fit,
    Scaled,
    Nup2,
    Nup4,
    Nup6,
    Nup9,
    Nup16,
    #[value(name = "poster2x2")]
    Poster2x2,
    #[value(name = "poster3x3")]
    Poster3x3,
    Booklet,
}

impl From<LayoutArg> for PageLayout {
    fn from(arg: LayoutArg) -> Self {
        match arg {
            LayoutArg::Normal => PageLayout::Normal,
            LayoutArg::Fit => PageLayout::FitToPage,
            LayoutArg::Scaled => PageLayout::Scaled { percent: 100 },
            LayoutArg::Nup2 => PageLayout::NUp { pages_per_sheet: 2 },
            LayoutArg::Nup4 => PageLayout::NUp { pages_per_sheet: 4 },
            LayoutArg::Nup6 => PageLayout::NUp { pages_per_sheet: 6 },
            LayoutArg::Nup9 => PageLayout::NUp { pages_per_sheet: 9 },
            LayoutArg::Nup16 => PageLayout::NUp {
                pages_per_sheet: 16,
            },
            LayoutArg::Poster2x2 => PageLayout::Poster { rows: 2, cols: 2 },
            LayoutArg::Poster3x3 => PageLayout::Poster { rows: 3, cols: 3 },
            LayoutArg::Booklet => PageLayout::Booklet,
        }
    }
}

#[derive(ValueEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum DryingArg {
    Short,
    Normal,
    Long,
    Extralong,
}

impl From<DryingArg> for InkDryingTime {
    fn from(arg: DryingArg) -> Self {
        match arg {
            DryingArg::Short => InkDryingTime::Short,
            DryingArg::Normal => InkDryingTime::Normal,
            DryingArg::Long => InkDryingTime::Long,
            DryingArg::Extralong => InkDryingTime::ExtraLong,
        }
    }
}

/// Convert quality integer (1..=5) to `PrintQuality`.
pub fn parse_quality_level(q: u8) -> PrintQuality {
    match q {
        1 => PrintQuality::Level1Fine,
        2 => PrintQuality::Level2High,
        3 => PrintQuality::Level3Standard,
        4 => PrintQuality::Level4Fast,
        _ => PrintQuality::Level5Draft,
    }
}

/// Convert quiet flag to `QuietMode`.
pub fn parse_quiet_mode(quiet: bool) -> QuietMode {
    if quiet {
        QuietMode::AlwaysOn
    } else {
        QuietMode::Off
    }
}

/// Convert wide_gap flag to `PaperGapMode`.
pub fn parse_paper_gap_mode(wide: bool) -> PaperGapMode {
    if wide {
        PaperGapMode::WidePreventAbrasion
    } else {
        PaperGapMode::Auto
    }
}

/// Parses the 14 alignment CSV integer offsets (-5..=5).
pub fn parse_alignment_values_csv(csv: &str) -> Result<Vec<i8>, String> {
    let parts: Vec<&str> = csv.split(',').map(|s| s.trim()).collect();
    if parts.len() != 14 {
        return Err(format!(
            "Expected exactly 14 comma-separated values for columns A..N, got {}",
            parts.len()
        ));
    }
    let mut values = Vec::with_capacity(14);
    for (i, part) in parts.iter().enumerate() {
        let col = (b'A' + i as u8) as char;
        let v: i8 = part
            .parse::<i8>()
            .map_err(|_| format!("Invalid integer '{part}' for column {col}"))?;
        if !(-5..=5).contains(&v) {
            return Err(format!(
                "Value {v} for column {col} out of range (must be -5..=5)"
            ));
        }
        values.push(v);
    }
    Ok(values)
}
