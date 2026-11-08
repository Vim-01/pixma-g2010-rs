//! Full printing preferences and page setup configuration matching the
//! official Windows Canon PIXMA G2010 MP Drivers.

use serde::{Deserialize, Serialize};

/// All media types supported by the Canon PIXMA G2010 firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum MediaType {
    /// Plain Paper (`stationery`) — uses Pigment Black (PGBK) for text.
    Plain,
    /// Photo Paper Plus Glossy II (`custom-media-type-canon-6`) — uses composite CMY (0% PGBK).
    PhotoPlusGlossyII,
    /// Photo Paper Pro Platinum (`custom-media-type-canon-3`) — composite CMY.
    PhotoProPlatinum,
    /// Photo Paper Pro Luster (`custom-media-type-canon-17`) — composite CMY.
    PhotoProLuster,
    /// Photo Paper Plus Semi-gloss (`custom-media-type-canon-9`) — composite CMY.
    PhotoPlusSemiGloss,
    /// Glossy Photo Paper (`photographic-glossy`) — composite CMY.
    GlossyPhoto,
    /// Matte Photo Paper (`custom-media-type-canon-11`).
    MattePhoto,
    /// High Resolution Paper (`custom-media-type-canon-18`).
    HighResolutionPaper,
    /// Envelope (`custom-media-type-canon-12`) — widens head platen gap.
    Envelope,
    /// Inkjet Hagaki postcard (`custom-media-type-canon-14`).
    InkjetHagaki,
    /// Standard Hagaki postcard (`custom-media-type-canon-15`).
    Hagaki,
    /// T-Shirt Transfers.
    TShirtTransfer,
    /// Greeting Card / Cardstock.
    Cardstock,
}

impl MediaType {
    /// Returns the exact `<ivec:mediatype>` XML string expected by the G2010 firmware.
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::Plain => "stationery",
            Self::GlossyPhoto => "photographic-glossy",
            Self::PhotoProPlatinum => "custom-media-type-canon-3",
            Self::PhotoPlusGlossyII => "custom-media-type-canon-6",
            Self::PhotoPlusSemiGloss => "custom-media-type-canon-9",
            Self::MattePhoto => "custom-media-type-canon-11",
            Self::Envelope => "custom-media-type-canon-12",
            Self::InkjetHagaki => "custom-media-type-canon-14",
            Self::Hagaki => "custom-media-type-canon-15",
            Self::PhotoProLuster => "custom-media-type-canon-17",
            Self::HighResolutionPaper | Self::TShirtTransfer | Self::Cardstock => {
                "custom-media-type-canon-18"
            }
        }
    }

    /// Returns `true` if this media has a glossy/microporous coating where Pigment Black (PGBK)
    /// cannot adhere and must be shut off in favor of composite CMY dye black.
    pub const fn requires_composite_cmy_black(self) -> bool {
        matches!(
            self,
            Self::PhotoPlusGlossyII
                | Self::PhotoProPlatinum
                | Self::PhotoProLuster
                | Self::PhotoPlusSemiGloss
                | Self::GlossyPhoto
        )
    }

    /// Parse from PPD or CLI option string.
    pub fn from_option_str(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "plain" | "stationery" => Self::Plain,
            "glossygold" | "plusglossyii" | "photoplusglossyii" => Self::PhotoPlusGlossyII,
            "proplat" | "photoproplatinum" => Self::PhotoProPlatinum,
            "luster" | "proluster" | "photoproluster" => Self::PhotoProLuster,
            "semigloss" | "photoplussemigloss" => Self::PhotoPlusSemiGloss,
            "glossypaper" | "glossy" | "photographic-glossy" => Self::GlossyPhoto,
            "matte" | "mattephoto" => Self::MattePhoto,
            "highres" | "highresolution" => Self::HighResolutionPaper,
            "envelope" => Self::Envelope,
            "ijpostcard" | "inkjethagaki" => Self::InkjetHagaki,
            "postcard" | "hagaki" => Self::Hagaki,
            "tshirt" => Self::TShirtTransfer,
            "cardstock" | "greetingcard" => Self::Cardstock,
            _ => Self::Plain,
        }
    }
}

/// Print quality levels (1 = Finest to 5 = Fast Draft).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PrintQuality {
    /// Level 1: Ultra Fine (1200×1200 / 4800×1200 equivalent).
    Level1Fine,
    /// Level 2: High Quality (`best`, 600×600 multi-pass).
    Level2High,
    /// Level 3: Standard (`normal`, 600×600).
    Level3Standard,
    /// Level 4: Fast Standard (`normal` high-speed pass).
    Level4Fast,
    /// Level 5: Economy / Draft (`fast_draft`, 300×300 base resolution).
    Level5Draft,
}

impl PrintQuality {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::Level1Fine | Self::Level2High => "best",
            Self::Level3Standard | Self::Level4Fast => "normal",
            Self::Level5Draft => "fast_draft",
        }
    }

    pub const fn base_raster_dpi(self) -> u32 {
        match self {
            Self::Level1Fine => 1200,
            Self::Level2High | Self::Level3Standard | Self::Level4Fast => 600,
            Self::Level5Draft => 300,
        }
    }
}

/// Standard and borderless paper sizes supported by the G2010.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperSize {
    A4,
    A5,
    A6,
    B5,
    Letter,
    Legal,
    Executive,
    Photo4x6,
    Photo5x7,
    Photo8x10,
    PhotoL,
    Square5x5,
    BusinessCard,
    EnvelopeCom10,
    EnvelopeDL,
    Hagaki,
    Custom { width_mm: u32, height_mm: u32 },
}

impl PaperSize {
    /// Dimensions in millimeters `(width_mm, height_mm)`.
    pub const fn dimensions_mm(self) -> (f32, f32) {
        match self {
            Self::A4 => (210.0, 297.0),
            Self::A5 => (148.0, 210.0),
            Self::A6 => (105.0, 148.0),
            Self::B5 => (182.0, 257.0),
            Self::Letter => (215.9, 279.4),
            Self::Legal => (215.9, 355.6),
            Self::Executive => (184.2, 266.7),
            Self::Photo4x6 => (101.6, 152.4),
            Self::Photo5x7 => (127.0, 177.8),
            Self::Photo8x10 => (203.2, 254.0),
            Self::PhotoL => (89.0, 127.0),
            Self::Square5x5 => (127.0, 127.0),
            Self::BusinessCard => (55.0, 91.0),
            Self::EnvelopeCom10 => (104.8, 241.3),
            Self::EnvelopeDL => (110.0, 220.0),
            Self::Hagaki => (100.0, 148.0),
            Self::Custom {
                width_mm,
                height_mm,
            } => (width_mm as f32, height_mm as f32),
        }
    }

    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::A4 => "iso_a4_210x297mm",
            Self::A5 => "iso_a5_148x210mm",
            Self::A6 => "iso_a6_105x148mm",
            Self::B5 => "jis_b5_182x257mm",
            Self::Letter => "na_letter_8.5x11in",
            Self::Legal => "na_legal_8.5x14in",
            Self::Executive => "na_executive_7.25x10.5in",
            Self::Photo4x6 => "na_index-4x6_4x6in",
            Self::Photo5x7 => "na_5x7_5x7in",
            Self::Photo8x10 => "na_govt-letter_8x10in",
            Self::PhotoL => "oe_photo-l_3.5x5in",
            Self::Square5x5 => "oe_square-5x5_5x5in",
            Self::BusinessCard => "oe_business-card_55x91mm",
            Self::EnvelopeCom10 => "na_number-10_4.125x9.5in",
            Self::EnvelopeDL => "iso_dl_110x220mm",
            Self::Hagaki => "jpn_hagaki_100x148mm",
            Self::Custom { .. } => "iso_a4_210x297mm",
        }
    }

    /// Returns the exact printable dot dimensions `(width_dots, height_dots)`
    /// specified by Canon firmware GetCapabilityResponse for the given DPI and borderless mode.
    pub const fn printable_area_dots(self, dpi: u32, borderless: bool) -> (u32, u32) {
        let (w_600, h_600) = match (self, borderless) {
            (Self::A4, false) => (4800, 6826),
            (Self::A4, true) => (5092, 7181),
            (Self::Letter, false) => (4800, 6411),
            (Self::Letter, true) => (5232, 6765),
            (Self::Photo4x6, false) => (2239, 3411),
            (Self::Photo4x6, true) => (2532, 3765),
            (Self::Photo5x7, false) => (2839, 4011),
            (Self::Photo5x7, true) => (3132, 4365),
            (Self::A5, _) => (3335, 4771),
            (Self::B5, _) => (4138, 5881),
            (Self::Legal, _) => (4800, 8211),
            (Self::Hagaki, false) => (2201, 3307),
            (Self::Hagaki, true) => (2494, 3661),
            _ => {
                let (w_mm, h_mm) = self.dimensions_mm();
                let printable_w_mm = if borderless {
                    w_mm
                } else {
                    let w = w_mm - 6.8;
                    if w < 10.0 {
                        10.0
                    } else {
                        w
                    }
                };
                let printable_h_mm = if borderless {
                    h_mm
                } else {
                    let h = h_mm - 8.0;
                    if h < 10.0 {
                        10.0
                    } else {
                        h
                    }
                };
                (
                    ((printable_w_mm * 600.0) / 25.4 + 0.5) as u32,
                    ((printable_h_mm * 600.0) / 25.4 + 0.5) as u32,
                )
            }
        };

        if dpi == 600 {
            (w_600, h_600)
        } else {
            let scale = dpi as f32 / 600.0;
            (
                (w_600 as f32 * scale + 0.5) as u32,
                (h_600 as f32 * scale + 0.5) as u32,
            )
        }
    }
}

/// Borderless printing mode and extension amount (0 = none to 3 = maximum overscan).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BorderlessExtension {
    /// Standard bordered printing (3mm top, 5mm bottom, 3.4mm side margins).
    Off,
    /// Borderless with zero overscan extension (Level 0).
    Extension0Min,
    /// Borderless with small overscan extension (Level 1).
    Extension1Low,
    /// Borderless with medium overscan extension (Level 2).
    Extension2Med,
    /// Borderless with maximum overscan extension (Level 3).
    Extension3Max,
}

impl BorderlessExtension {
    pub const fn is_borderless(self) -> bool {
        !matches!(self, Self::Off)
    }

    /// Overscan scale factor applied to expand the image slightly past paper edges.
    pub const fn overscan_factor(self) -> f32 {
        match self {
            Self::Off | Self::Extension0Min => 1.0,
            Self::Extension1Low => 1.012,
            Self::Extension2Med => 1.024,
            Self::Extension3Max => 1.036,
        }
    }
}

/// Manual color and intensity controls matching the Windows Color/Intensity dialog.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct ColorAdjustment {
    /// Cyan balance (-50..=+50).
    pub cyan_balance: i8,
    /// Magenta balance (-50..=+50).
    pub magenta_balance: i8,
    /// Yellow balance (-50..=+50).
    pub yellow_balance: i8,
    /// Brightness adjustment (-50..=+50; corresponds to Light/Normal/Dark).
    pub brightness: i8,
    /// Contrast adjustment (-50..=+50).
    pub contrast: i8,
    /// Overall ink intensity (-50..=+50).
    pub intensity: i8,
    /// Gamma target (e.g. 1.4, 1.8, 2.2).
    pub gamma: f32,
    /// Grayscale mode (monochrome printing).
    pub grayscale: bool,
}

impl Default for ColorAdjustment {
    fn default() -> Self {
        Self {
            cyan_balance: 0,
            magenta_balance: 0,
            yellow_balance: 0,
            brightness: 0,
            contrast: 0,
            intensity: 0,
            gamma: 2.2,
            grayscale: false,
        }
    }
}

/// Page layout mode (Normal, Fit-to-Page, Scaled, N-up, Poster, Booklet).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageLayout {
    Normal,
    FitToPage,
    Scaled { percent: u16 },
    NUp { pages_per_sheet: u8 },
    Poster { rows: u8, cols: u8 },
    Booklet,
}

/// Manual duplex settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DuplexMode {
    Off,
    LongEdge,
    ShortEdge,
}

/// Ink drying wait time between pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InkDryingTime {
    Short,
    Normal,
    Long,
    ExtraLong,
}

impl InkDryingTime {
    pub const fn pause_seconds(self) -> u64 {
        match self {
            Self::Short => 0,
            Self::Normal => 1,
            Self::Long => 5,
            Self::ExtraLong => 12,
        }
    }
}

/// Head height / paper abrasion prevention mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PaperGapMode {
    Auto,
    WidePreventAbrasion,
}

impl PaperGapMode {
    pub const fn ivec_xml_value(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::WidePreventAbrasion => "wide",
        }
    }
}

/// Acoustic quiet mode schedule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuietSchedule {
    pub start_hour: u8,
    pub start_minute: u8,
    pub end_hour: u8,
    pub end_minute: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuietMode {
    Off,
    AlwaysOn,
    Scheduled(QuietSchedule),
}

/// Complete print job configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PrintJobConfig {
    pub job_id: u32,
    pub media_type: MediaType,
    pub paper_size: PaperSize,
    pub quality: PrintQuality,
    pub borderless: BorderlessExtension,
    pub color: ColorAdjustment,
    pub layout: PageLayout,
    pub duplex: DuplexMode,
    pub drying_time: InkDryingTime,
    pub paper_gap: PaperGapMode,
    pub quiet_mode: QuietMode,
    pub copies: u16,
    pub collate: bool,
    pub reverse_order: bool,
}

impl Default for PrintJobConfig {
    fn default() -> Self {
        Self {
            job_id: 2,
            media_type: MediaType::Plain,
            paper_size: PaperSize::A4,
            quality: PrintQuality::Level3Standard,
            borderless: BorderlessExtension::Off,
            color: ColorAdjustment::default(),
            layout: PageLayout::Normal,
            duplex: DuplexMode::Off,
            drying_time: InkDryingTime::Normal,
            paper_gap: PaperGapMode::Auto,
            quiet_mode: QuietMode::Off,
            copies: 1,
            collate: false,
            reverse_order: false,
        }
    }
}
