//! Native Wayland/X11 GTK4 + Libadwaita application for PIXMA G2010 Studio.

use pixma_core::maintenance::{CleaningTarget, CleaningType, RollerCleanTarget, TestPrintType};
use pixma_core::models::CartridgeState;
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintQuality, QuietMode,
};
use pixma_core::scan_params::{ScanColorMode, ScanResolution};
use pixma_daemon::DaemonClient;
use relm4::adw::prelude::*;
use relm4::{adw, gtk, ComponentParts, ComponentSender, SimpleComponent};
use std::sync::Arc;
use tokio::sync::Mutex as TokioMutex;

use crate::state::{StudioEvent, StudioViewModel, GLOSSY_COMPOSITE_CMY_NOTICE};

/// Application messages processed by the Relm4 runtime loop.
#[derive(Debug)]
pub enum AppMsg {
    // Synchronous UI updates
    MediaTypeChanged(usize),
    PrintQualityChanged(usize),
    PaperSizeChanged(usize),
    BorderlessChanged(usize),
    PageLayoutChanged(usize),
    GrayscaleToggled(bool),
    CyanBalanceChanged(f64),
    MagentaBalanceChanged(f64),
    YellowBalanceChanged(f64),
    BrightnessChanged(f64),
    ContrastChanged(f64),
    PaperGapToggled(bool),
    DryingTimeChanged(usize),
    QuietModeToggled(bool),

    ScanResolutionChanged(usize),
    ScanColorModeChanged(usize),
    ScanAutoCropToggled(bool),
    ScanDeskewToggled(bool),
    ScanDescreenToggled(bool),
    ScanUnsharpToggled(bool),
    ScanRemoveDustToggled(bool),
    ScanGutterShadowToggled(bool),
    ScanGammaChanged(f64),

    AlignmentColumnChanged(usize, i8),
    InkFlushConfirmationToggled(bool),

    // Asynchronous hardware actions
    TriggerRefreshTelemetry,
    TriggerPrintTestPage,
    TriggerClean(CleaningType, CleaningTarget),
    TriggerMechanicalClean(RollerCleanTarget),
    TriggerTestPrint(TestPrintType),
    TriggerApplyAlignment,
    TriggerScan,

    // Task completions
    HardwareTaskFinished(Result<String, String>),
}

/// Main application model holding the state view-model and hardware client.
pub struct AppModel {
    pub vm: StudioViewModel,
    pub client: Option<Arc<TokioMutex<DaemonClient>>>,
}

/// Collection of widgets that need real-time updates when state changes.
pub struct AppWidgets {
    // Print settings
    pub print_summary_label: gtk::Label,
    pub glossy_notice_label: gtk::Label,

    // Maintenance
    pub flush_execute_btn: gtk::Button,
    pub flush_confirm_switch: gtk::Switch,
    pub align_scales: Vec<gtk::Scale>,

    // Status monitor
    pub pgbk_bar: gtk::LevelBar,
    pub pgbk_label: gtk::Label,
    pub cyan_bar: gtk::LevelBar,
    pub cyan_label: gtk::Label,
    pub magenta_bar: gtk::LevelBar,
    pub magenta_label: gtk::Label,
    pub yellow_bar: gtk::LevelBar,
    pub yellow_label: gtk::Label,
    pub absorber_bar: gtk::LevelBar,
    pub absorber_label: gtk::Label,
    pub absorber_warning_label: gtk::Label,
    pub black_cartridge_row: adw::ActionRow,
    pub color_cartridge_row: adw::ActionRow,
    pub diagnostic_banner_row: adw::ActionRow,

    // Global
    pub toast_overlay: adw::ToastOverlay,
}

impl SimpleComponent for AppModel {
    type Input = AppMsg;
    type Output = ();
    type Init = Option<DaemonClient>;
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .title("PIXMA G2010 Studio")
            .default_width(920)
            .default_height(780)
            .build()
    }

    fn init(
        client_opt: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let vm = StudioViewModel::default();
        let client = client_opt.map(|c| Arc::new(TokioMutex::new(c)));
        let model = Self { vm, client };

        let toast_overlay = adw::ToastOverlay::new();

        let main_box = gtk::Box::new(gtk::Orientation::Vertical, 0);

        // HeaderBar with embedded ViewSwitcher
        let header_bar = adw::HeaderBar::new();
        let view_stack = adw::ViewStack::new();

        let view_switcher = adw::ViewSwitcher::builder()
            .stack(&view_stack)
            .policy(adw::ViewSwitcherPolicy::Wide)
            .build();
        header_bar.set_title_widget(Some(&view_switcher));
        main_box.append(&header_bar);

        // -------------------------------------------------------------------
        // TAB 1: Print Settings
        // -------------------------------------------------------------------
        let print_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
        print_box.set_margin_top(16);
        print_box.set_margin_bottom(16);
        print_box.set_margin_start(16);
        print_box.set_margin_end(16);

        let print_summary_label = gtk::Label::builder()
            .label(model.vm.ui_print_summary())
            .css_classes(["title-4"])
            .wrap(true)
            .xalign(0.0)
            .build();
        print_box.append(&print_summary_label);

        let glossy_notice_label = gtk::Label::builder()
            .label(GLOSSY_COMPOSITE_CMY_NOTICE)
            .css_classes(["warning", "caption"])
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        print_box.append(&glossy_notice_label);

        let media_group = adw::PreferencesGroup::builder()
            .title("Media & Quality")
            .description("Configure print media, resolution passes, and page size")
            .build();

        // Media Type DropDown
        let media_row = adw::ActionRow::builder()
            .title("Media Type")
            .subtitle("Select paper stock and coating")
            .build();
        let media_dropdown = gtk::DropDown::from_strings(&[
            "Plain Paper (GI-790 PGBK)",
            "Photo Paper Plus Glossy II (Composite CMY)",
            "Photo Paper Pro Platinum (Composite CMY)",
            "Photo Paper Pro Luster (Composite CMY)",
            "Photo Paper Plus Semi-gloss (Composite CMY)",
            "Glossy Photo Paper (Composite CMY)",
            "Matte Photo Paper",
            "High Resolution Paper",
            "Envelope",
            "Hagaki Postcard",
            "Cardstock",
        ]);
        let s_media = sender.clone();
        media_dropdown.connect_selected_notify(move |dd| {
            s_media.input(AppMsg::MediaTypeChanged(dd.selected() as usize));
        });
        media_row.add_suffix(&media_dropdown);
        media_group.add(&media_row);

        // Print Quality DropDown
        let quality_row = adw::ActionRow::builder()
            .title("Print Quality")
            .subtitle("Fine (1200 DPI) to Draft (300 DPI)")
            .build();
        let quality_dropdown = gtk::DropDown::from_strings(&[
            "Level 1 - Ultra Fine (1200×1200 DPI)",
            "Level 2 - High Quality (600×600 multi-pass)",
            "Level 3 - Standard Normal (600×600 DPI)",
            "Level 4 - Fast Normal (600×600 high speed)",
            "Level 5 - Fast Draft (300×300 base DPI)",
        ]);
        quality_dropdown.set_selected(2); // Level 3 Standard
        let s_qual = sender.clone();
        quality_dropdown.connect_selected_notify(move |dd| {
            s_qual.input(AppMsg::PrintQualityChanged(dd.selected() as usize));
        });
        quality_row.add_suffix(&quality_dropdown);
        media_group.add(&quality_row);

        // Paper Size DropDown
        let size_row = adw::ActionRow::builder()
            .title("Paper Size")
            .subtitle("Target sheet dimensions")
            .build();
        let size_dropdown = gtk::DropDown::from_strings(&[
            "A4 (210 × 297 mm)",
            "A5 (148 × 210 mm)",
            "A6 (105 × 148 mm)",
            "JIS B5 (182 × 257 mm)",
            "Letter (8.5 × 11 in)",
            "Legal (8.5 × 14 in)",
            "Executive (7.25 × 10.5 in)",
            "Photo 4×6 in (10 × 15 cm)",
            "Photo 5×7 in (13 × 18 cm)",
            "Photo 8×10 in",
            "Photo L (89 × 127 mm)",
            "Square 5×5 in",
            "Business Card (55 × 91 mm)",
            "Envelope #10",
            "Envelope DL",
            "Hagaki (100 × 148 mm)",
        ]);
        let s_size = sender.clone();
        size_dropdown.connect_selected_notify(move |dd| {
            s_size.input(AppMsg::PaperSizeChanged(dd.selected() as usize));
        });
        size_row.add_suffix(&size_dropdown);
        media_group.add(&size_row);

        // Borderless DropDown
        let borderless_row = adw::ActionRow::builder()
            .title("Borderless Printing")
            .subtitle("Overscan bleeding expansion")
            .build();
        let borderless_dropdown = gtk::DropDown::from_strings(&[
            "Off (Standard 3mm-5mm margins)",
            "Extension 0 (Zero overscan)",
            "Extension 1 (Small overscan +1.2%)",
            "Extension 2 (Medium overscan +2.4%)",
            "Extension 3 (Maximum overscan +3.6%)",
        ]);
        let s_border = sender.clone();
        borderless_dropdown.connect_selected_notify(move |dd| {
            s_border.input(AppMsg::BorderlessChanged(dd.selected() as usize));
        });
        borderless_row.add_suffix(&borderless_dropdown);
        media_group.add(&borderless_row);

        // Page Layout DropDown
        let layout_row = adw::ActionRow::builder()
            .title("Page Layout")
            .subtitle("Page imposition and N-up tiling")
            .build();
        let layout_dropdown = gtk::DropDown::from_strings(&[
            "Normal (1 page per sheet)",
            "Fit to Page",
            "Scaled (100%)",
            "2-up (2 pages per sheet)",
            "4-up (4 pages per sheet)",
            "6-up (6 pages per sheet)",
            "9-up (9 pages per sheet)",
            "16-up (16 pages per sheet)",
            "Poster 2×2 (4 sheets)",
            "Poster 3×3 (9 sheets)",
            "Booklet (Saddle-stitch)",
        ]);
        let s_layout = sender.clone();
        layout_dropdown.connect_selected_notify(move |dd| {
            s_layout.input(AppMsg::PageLayoutChanged(dd.selected() as usize));
        });
        layout_row.add_suffix(&layout_dropdown);
        media_group.add(&layout_row);

        // Paper Gap Switch (Prevent Paper Abrasion)
        let gap_row = adw::ActionRow::builder()
            .title("Prevent Paper Abrasion")
            .subtitle("Widens carriage head-to-platen gap for thick media")
            .build();
        let gap_switch = gtk::Switch::new();
        let s_gap = sender.clone();
        gap_switch.connect_active_notify(move |sw| {
            s_gap.input(AppMsg::PaperGapToggled(sw.is_active()));
        });
        gap_row.add_suffix(&gap_switch);
        media_group.add(&gap_row);

        // Ink Drying Wait Time
        let drying_row = adw::ActionRow::builder()
            .title("Ink Drying Wait Time")
            .subtitle("Inter-sheet pause for high-coverage saturation")
            .build();
        let drying_dropdown = gtk::DropDown::from_strings(&[
            "Short (0s)",
            "Normal (1s)",
            "Long (5s)",
            "Extra Long (12s)",
        ]);
        drying_dropdown.set_selected(1);
        let s_dry = sender.clone();
        drying_dropdown.connect_selected_notify(move |dd| {
            s_dry.input(AppMsg::DryingTimeChanged(dd.selected() as usize));
        });
        drying_row.add_suffix(&drying_dropdown);
        media_group.add(&drying_row);

        // Quiet Mode Switch
        let quiet_row = adw::ActionRow::builder()
            .title("Acoustic Quiet Mode")
            .subtitle("Slows carriage acceleration to reduce operating noise")
            .build();
        let quiet_switch = gtk::Switch::new();
        let s_quiet = sender.clone();
        quiet_switch.connect_active_notify(move |sw| {
            s_quiet.input(AppMsg::QuietModeToggled(sw.is_active()));
        });
        quiet_row.add_suffix(&quiet_switch);
        media_group.add(&quiet_row);

        print_box.append(&media_group);

        // Color & Tone Controls PreferencesGroup
        let color_group = adw::PreferencesGroup::builder()
            .title("Color Balance & Tone Adjustments")
            .description("Manual color balance (-50..50) and monochrome conversion")
            .build();

        let gray_row = adw::ActionRow::builder()
            .title("Grayscale Monochrome")
            .subtitle("Converts image to SGray before rasterization")
            .build();
        let gray_switch = gtk::Switch::new();
        let s_gray = sender.clone();
        gray_switch.connect_active_notify(move |sw| {
            s_gray.input(AppMsg::GrayscaleToggled(sw.is_active()));
        });
        gray_row.add_suffix(&gray_switch);
        color_group.add(&gray_row);

        // Helper macro for scale rows
        let make_scale_row =
            |title: &'static str, default: f64, msg_fn: Box<dyn Fn(f64) -> AppMsg + 'static>| {
                let row = adw::ActionRow::builder().title(title).build();
                let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -50.0, 50.0, 1.0);
                scale.set_value(default);
                scale.set_width_request(200);
                scale.set_draw_value(true);
                scale.set_value_pos(gtk::PositionType::Right);
                let s = sender.clone();
                scale.connect_value_changed(move |sc| {
                    s.input(msg_fn(sc.value()));
                });
                row.add_suffix(&scale);
                row
            };

        color_group.add(&make_scale_row(
            "Cyan Balance",
            0.0,
            Box::new(AppMsg::CyanBalanceChanged),
        ));
        color_group.add(&make_scale_row(
            "Magenta Balance",
            0.0,
            Box::new(AppMsg::MagentaBalanceChanged),
        ));
        color_group.add(&make_scale_row(
            "Yellow Balance",
            0.0,
            Box::new(AppMsg::YellowBalanceChanged),
        ));
        color_group.add(&make_scale_row(
            "Brightness",
            0.0,
            Box::new(AppMsg::BrightnessChanged),
        ));
        color_group.add(&make_scale_row(
            "Contrast",
            0.0,
            Box::new(AppMsg::ContrastChanged),
        ));

        print_box.append(&color_group);

        let print_test_btn = gtk::Button::builder()
            .label("Print Diagnostic Calibration Test Page")
            .css_classes(["suggested-action", "pill"])
            .margin_top(8)
            .build();
        let s_test = sender.clone();
        print_test_btn.connect_clicked(move |_| {
            s_test.input(AppMsg::TriggerPrintTestPage);
        });
        print_box.append(&print_test_btn);

        let print_scroll = gtk::ScrolledWindow::builder()
            .child(&print_box)
            .vexpand(true)
            .build();

        let print_page = view_stack.add_titled(&print_scroll, Some("print"), "Print Settings");
        print_page.set_icon_name(Some("printer-symbolic"));

        // -------------------------------------------------------------------
        // TAB 2: Maintenance Assistant
        // -------------------------------------------------------------------
        let maint_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        maint_box.set_margin_top(16);
        maint_box.set_margin_bottom(16);
        maint_box.set_margin_start(16);
        maint_box.set_margin_end(16);

        // Head Cleaning Group
        let clean_group = adw::PreferencesGroup::builder()
            .title("Print Head Cleaning")
            .description(
                "Cleans microscopic FINE nozzle orifices to resolve banding and missing lines",
            )
            .build();

        let regular_row = adw::ActionRow::builder()
            .title("Standard Cleaning (~0.3 ml ink, 60s cooldown)")
            .subtitle("Unclogs minor nozzle blockages")
            .build();
        let btn_reg_all = gtk::Button::with_label("All");
        let s_c1 = sender.clone();
        btn_reg_all.connect_clicked(move |_| {
            s_c1.input(AppMsg::TriggerClean(
                CleaningType::Regular,
                CleaningTarget::All,
            ));
        });
        let btn_reg_bk = gtk::Button::with_label("Black");
        let s_c2 = sender.clone();
        btn_reg_bk.connect_clicked(move |_| {
            s_c2.input(AppMsg::TriggerClean(
                CleaningType::Regular,
                CleaningTarget::BlackOnly,
            ));
        });
        let btn_reg_col = gtk::Button::with_label("Color");
        let s_c3 = sender.clone();
        btn_reg_col.connect_clicked(move |_| {
            s_c3.input(AppMsg::TriggerClean(
                CleaningType::Regular,
                CleaningTarget::ColorOnly,
            ));
        });
        let reg_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        reg_btn_box.append(&btn_reg_all);
        reg_btn_box.append(&btn_reg_bk);
        reg_btn_box.append(&btn_reg_col);
        regular_row.add_suffix(&reg_btn_box);
        clean_group.add(&regular_row);

        let deep_row = adw::ActionRow::builder()
            .title("Deep Cleaning (~2.0 ml ink, 10 min cooldown)")
            .subtitle("Use if standard cleaning fails to clear nozzles")
            .build();
        let btn_deep_all = gtk::Button::with_label("All");
        let s_d1 = sender.clone();
        btn_deep_all.connect_clicked(move |_| {
            s_d1.input(AppMsg::TriggerClean(
                CleaningType::Deep,
                CleaningTarget::All,
            ));
        });
        let btn_deep_bk = gtk::Button::with_label("Black");
        let s_d2 = sender.clone();
        btn_deep_bk.connect_clicked(move |_| {
            s_d2.input(AppMsg::TriggerClean(
                CleaningType::Deep,
                CleaningTarget::BlackOnly,
            ));
        });
        let btn_deep_col = gtk::Button::with_label("Color");
        let s_d3 = sender.clone();
        btn_deep_col.connect_clicked(move |_| {
            s_d3.input(AppMsg::TriggerClean(
                CleaningType::Deep,
                CleaningTarget::ColorOnly,
            ));
        });
        let deep_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        deep_btn_box.append(&btn_deep_all);
        deep_btn_box.append(&btn_deep_bk);
        deep_btn_box.append(&btn_deep_col);
        deep_row.add_suffix(&deep_btn_box);
        clean_group.add(&deep_row);

        maint_box.append(&clean_group);

        // System Ink Flush Group
        let flush_group = adw::PreferencesGroup::builder()
            .title("System Ink Flush (choke)")
            .description("High-pressure purge for air gaps in CISS tubes. Consumes ~45 ml ink and advances waste absorber by ~20%. Mandatory 24h cooldown. Hard-blocked if absorber >= 80%.")
            .build();

        let flush_confirm_row = adw::ActionRow::builder()
            .title("Safety Confirmation")
            .subtitle(
                "I understand Ink Flush consumes ~45 ml of ink and advances waste absorber counter",
            )
            .build();
        let flush_confirm_switch = gtk::Switch::new();
        let s_flush_toggle = sender.clone();
        flush_confirm_switch.connect_active_notify(move |sw| {
            s_flush_toggle.input(AppMsg::InkFlushConfirmationToggled(sw.is_active()));
        });
        flush_confirm_row.add_suffix(&flush_confirm_switch);
        flush_group.add(&flush_confirm_row);

        let flush_btn_row = adw::ActionRow::builder()
            .title("Execute System Ink Flush")
            .build();
        let flush_execute_btn = gtk::Button::builder()
            .label("Purge Tubes (Ink Flush)")
            .css_classes(["destructive-action"])
            .sensitive(false)
            .build();
        let s_flush = sender.clone();
        flush_execute_btn.connect_clicked(move |_| {
            s_flush.input(AppMsg::TriggerClean(
                CleaningType::InkFlush,
                CleaningTarget::All,
            ));
        });
        flush_btn_row.add_suffix(&flush_execute_btn);
        flush_group.add(&flush_btn_row);

        maint_box.append(&flush_group);

        // Mechanical Cleaning Group
        let mech_group = adw::PreferencesGroup::builder()
            .title("Mechanical Cleaning")
            .description("Paper path maintenance without ink consumption")
            .build();

        let roller_row = adw::ActionRow::builder()
            .title("Paper Feed Rollers")
            .subtitle("Cleans paper feed rubber pickup rollers")
            .build();
        let btn_roller = gtk::Button::with_label("Clean Rollers");
        let s_m1 = sender.clone();
        btn_roller.connect_clicked(move |_| {
            s_m1.input(AppMsg::TriggerMechanicalClean(
                RollerCleanTarget::FeedRoller,
            ));
        });
        roller_row.add_suffix(&btn_roller);
        mech_group.add(&roller_row);

        let platen_row = adw::ActionRow::builder()
            .title("Bottom Plate Platen")
            .subtitle("Cleans ink mist deposits on platen ribs")
            .build();
        let btn_platen = gtk::Button::with_label("Clean Platen");
        let s_m2 = sender.clone();
        btn_platen.connect_clicked(move |_| {
            s_m2.input(AppMsg::TriggerMechanicalClean(
                RollerCleanTarget::BottomPlate,
            ));
        });
        platen_row.add_suffix(&btn_platen);
        mech_group.add(&platen_row);

        maint_box.append(&mech_group);

        // Diagnostic Tests & Head Alignment Group
        let diag_group = adw::PreferencesGroup::builder()
            .title("Diagnostic Patterns & Head Alignment")
            .description("Nozzle inspection pattern and manual alignment offsets")
            .build();

        let nozzle_row = adw::ActionRow::builder()
            .title("Nozzle Check Pattern")
            .subtitle("Prints CMYK stepped density bars and staircase combs")
            .build();
        let btn_nozzle = gtk::Button::with_label("Print Pattern");
        let s_t1 = sender.clone();
        btn_nozzle.connect_clicked(move |_| {
            s_t1.input(AppMsg::TriggerTestPrint(TestPrintType::NozzleCheck));
        });
        nozzle_row.add_suffix(&btn_nozzle);
        diag_group.add(&nozzle_row);

        let align_sheet_row = adw::ActionRow::builder()
            .title("Alignment Sheet")
            .subtitle("Print patterns to calibrate head registration")
            .build();
        let btn_man_align = gtk::Button::with_label("Manual Sheet (2p)");
        let s_t2 = sender.clone();
        btn_man_align.connect_clicked(move |_| {
            s_t2.input(AppMsg::TriggerTestPrint(
                TestPrintType::ManualAlignmentPattern,
            ));
        });
        let btn_half_align = gtk::Button::with_label("Half-Auto Sheet (1p)");
        let s_t3 = sender.clone();
        btn_half_align.connect_clicked(move |_| {
            s_t3.input(AppMsg::TriggerTestPrint(TestPrintType::HalfAutoAlignment));
        });
        let align_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        align_btn_box.append(&btn_man_align);
        align_btn_box.append(&btn_half_align);
        align_sheet_row.add_suffix(&align_btn_box);
        diag_group.add(&align_sheet_row);

        // Manual alignment columns A..N
        let align_values_row = adw::ActionRow::builder()
            .title("Manual Alignment Offsets (Columns A..N)")
            .subtitle("Enter column values with least noticeable streaks (-5..=5)")
            .build();

        let mut align_scales = Vec::with_capacity(14);
        let columns_grid = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for col_idx in 0..14 {
            let col_letter = (b'A' + col_idx as u8) as char;
            let col_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
            let col_lbl = gtk::Label::builder()
                .label(format!("{col_letter}:"))
                .width_request(24)
                .build();
            let col_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, -5.0, 5.0, 1.0);
            col_scale.set_value(0.0);
            col_scale.set_draw_value(true);
            col_scale.set_width_request(160);
            let s_col = sender.clone();
            col_scale.connect_value_changed(move |sc| {
                s_col.input(AppMsg::AlignmentColumnChanged(
                    col_idx,
                    sc.value().round() as i8,
                ));
            });
            col_box.append(&col_lbl);
            col_box.append(&col_scale);
            columns_grid.append(&col_box);
            align_scales.push(col_scale);
        }
        align_values_row.add_suffix(&columns_grid);
        diag_group.add(&align_values_row);

        let apply_align_row = adw::ActionRow::builder()
            .title("Apply Manual Alignment")
            .subtitle("Transmits offset table A..N to printer NVRAM")
            .build();
        let btn_apply_align = gtk::Button::builder()
            .label("Apply Offsets")
            .css_classes(["suggested-action"])
            .build();
        let s_apply = sender.clone();
        btn_apply_align.connect_clicked(move |_| {
            s_apply.input(AppMsg::TriggerApplyAlignment);
        });
        apply_align_row.add_suffix(&btn_apply_align);
        diag_group.add(&apply_align_row);

        maint_box.append(&diag_group);

        let maint_scroll = gtk::ScrolledWindow::builder()
            .child(&maint_box)
            .vexpand(true)
            .build();

        let maint_page = view_stack.add_titled(&maint_scroll, Some("maintenance"), "Maintenance");
        maint_page.set_icon_name(Some("emblem-system-symbolic"));

        // -------------------------------------------------------------------
        // TAB 3: ScanGear Pro
        // -------------------------------------------------------------------
        let scan_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        scan_box.set_margin_top(16);
        scan_box.set_margin_bottom(16);
        scan_box.set_margin_start(16);
        scan_box.set_margin_end(16);

        let scan_opt_group = adw::PreferencesGroup::builder()
            .title("Flatbed Acquisition Parameters")
            .description("Optical resolution and color sampling depth")
            .build();

        let scan_res_row = adw::ActionRow::builder()
            .title("Scan Resolution")
            .subtitle("Optical and sensor-scaled DPI")
            .build();
        let scan_res_dropdown = gtk::DropDown::from_strings(&[
            "75 DPI (Preview)",
            "100 DPI",
            "150 DPI",
            "200 DPI",
            "300 DPI (Standard Document)",
            "400 DPI",
            "600 DPI (High Resolution Photo)",
            "1200 DPI (Maximum Optical)",
        ]);
        scan_res_dropdown.set_selected(4); // 300 DPI
        let s_sres = sender.clone();
        scan_res_dropdown.connect_selected_notify(move |dd| {
            s_sres.input(AppMsg::ScanResolutionChanged(dd.selected() as usize));
        });
        scan_res_row.add_suffix(&scan_res_dropdown);
        scan_opt_group.add(&scan_res_row);

        let scan_mode_row = adw::ActionRow::builder()
            .title("Color Mode")
            .subtitle("24-bit RGB, 8-bit Grayscale, or 1-bit Lineart")
            .build();
        let scan_mode_dropdown = gtk::DropDown::from_strings(&[
            "24-bit Color (8 bits per channel)",
            "8-bit Grayscale (Luminance)",
            "1-bit Black & White Lineart",
        ]);
        let s_smode = sender.clone();
        scan_mode_dropdown.connect_selected_notify(move |dd| {
            s_smode.input(AppMsg::ScanColorModeChanged(dd.selected() as usize));
        });
        scan_mode_row.add_suffix(&scan_mode_dropdown);
        scan_opt_group.add(&scan_mode_row);

        scan_box.append(&scan_opt_group);

        let filter_group = adw::PreferencesGroup::builder()
            .title("ScanGear Pro Restoration Filters")
            .description("Photodiode alignment, noise suppression, and geometry corrections")
            .build();

        let make_switch_row =
            |title: &'static str,
             sub: &'static str,
             initial: bool,
             msg_fn: Box<dyn Fn(bool) -> AppMsg + 'static>| {
                let row = adw::ActionRow::builder().title(title).subtitle(sub).build();
                let sw = gtk::Switch::new();
                sw.set_active(initial);
                let s = sender.clone();
                sw.connect_active_notify(move |w| {
                    s.input(msg_fn(w.is_active()));
                });
                row.add_suffix(&sw);
                row
            };

        filter_group.add(&make_switch_row(
            "Auto Multi-Crop",
            "Segments multiple photos placed on the platen into individual cropped files",
            false,
            Box::new(AppMsg::ScanAutoCropToggled),
        ));
        filter_group.add(&make_switch_row(
            "Document Deskew",
            "Detects edge skew angles and rotates document back to horizontal",
            false,
            Box::new(AppMsg::ScanDeskewToggled),
        ));
        filter_group.add(&make_switch_row(
            "2D-FFT Halftone Descreening",
            "Suppresses magazine and newsprint dot-matrix moiré interference patterns",
            false,
            Box::new(AppMsg::ScanDescreenToggled),
        ));
        filter_group.add(&make_switch_row(
            "Gaussian Unsharp Masking",
            "Enhances contrast around text characters and fine lines",
            true,
            Box::new(AppMsg::ScanUnsharpToggled),
        ));
        filter_group.add(&make_switch_row(
            "Dust & Scratch Removal",
            "Suppresses platen glass scratches and hair fibers via morphological median filtering",
            false,
            Box::new(AppMsg::ScanRemoveDustToggled),
        ));
        filter_group.add(&make_switch_row(
            "Gutter Shadow Correction",
            "Compensates for darkness and curvature shadows near book spines",
            false,
            Box::new(AppMsg::ScanGutterShadowToggled),
        ));

        let gamma_row = adw::ActionRow::builder()
            .title("Scanner Midtone Gamma")
            .subtitle("Tone curve exponent (default: 2.2)")
            .build();
        let gamma_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.5, 3.5, 0.1);
        gamma_scale.set_value(2.2);
        gamma_scale.set_draw_value(true);
        gamma_scale.set_width_request(180);
        let s_gamma = sender.clone();
        gamma_scale.connect_value_changed(move |sc| {
            s_gamma.input(AppMsg::ScanGammaChanged(sc.value()));
        });
        gamma_row.add_suffix(&gamma_scale);
        filter_group.add(&gamma_row);

        scan_box.append(&filter_group);

        let btn_scan = gtk::Button::builder()
            .label("Acquire Scan Document & Export")
            .css_classes(["suggested-action", "pill"])
            .margin_top(8)
            .build();
        let s_scan = sender.clone();
        btn_scan.connect_clicked(move |_| {
            s_scan.input(AppMsg::TriggerScan);
        });
        scan_box.append(&btn_scan);

        let scan_scroll = gtk::ScrolledWindow::builder()
            .child(&scan_box)
            .vexpand(true)
            .build();

        let scan_page = view_stack.add_titled(&scan_scroll, Some("scan"), "ScanGear Pro");
        scan_page.set_icon_name(Some("scanner-symbolic"));

        // -------------------------------------------------------------------
        // TAB 4: Live Status Monitor
        // -------------------------------------------------------------------
        let status_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        status_box.set_margin_top(16);
        status_box.set_margin_bottom(16);
        status_box.set_margin_start(16);
        status_box.set_margin_end(16);

        let diag_banner_group = adw::PreferencesGroup::new();
        let diagnostic_banner_row = adw::ActionRow::builder()
            .title("Status: Device Ready")
            .subtitle("No hardware support codes active")
            .css_classes(["card"])
            .build();
        diag_banner_group.add(&diagnostic_banner_row);
        status_box.append(&diag_banner_group);

        // GI-790 CISS Ink Levels
        let ink_group = adw::PreferencesGroup::builder()
            .title("GI-790 CISS Ink Tank Levels (Estimated)")
            .description("Droplet usage accounting & firmware low-ink markers")
            .build();

        let make_gauge_row = |name: &'static str| {
            let row = adw::ActionRow::builder().title(name).build();
            let box_h = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let bar = gtk::LevelBar::builder()
                .min_value(0.0)
                .max_value(1.0)
                .value(1.0)
                .width_request(180)
                .build();
            let lbl = gtk::Label::builder()
                .label("100%")
                .width_request(45)
                .build();
            box_h.append(&bar);
            box_h.append(&lbl);
            row.add_suffix(&box_h);
            (row, bar, lbl)
        };

        let (row_pgbk, pgbk_bar, pgbk_label) = make_gauge_row("Pigment Black (PGBK)");
        let (row_c, cyan_bar, cyan_label) = make_gauge_row("Cyan (C)");
        let (row_m, magenta_bar, magenta_label) = make_gauge_row("Magenta (M)");
        let (row_y, yellow_bar, yellow_label) = make_gauge_row("Yellow (Y)");

        ink_group.add(&row_pgbk);
        ink_group.add(&row_c);
        ink_group.add(&row_m);
        ink_group.add(&row_y);
        status_box.append(&ink_group);

        // Waste Ink Absorber
        let absorber_group = adw::PreferencesGroup::builder()
            .title("Waste Ink Absorber (D-Counter)")
            .description("Internal felt pad saturation. Ink Flush blocked at >= 80%. Fatal error 5B00/P07 at 100%.")
            .build();

        let absorber_row = adw::ActionRow::builder().title("Saturation Level").build();
        let abs_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let absorber_bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(1.0)
            .value(0.0)
            .width_request(180)
            .build();
        let absorber_label = gtk::Label::builder()
            .label("0.0%")
            .width_request(55)
            .build();
        abs_box.append(&absorber_bar);
        abs_box.append(&absorber_label);
        absorber_row.add_suffix(&abs_box);
        absorber_group.add(&absorber_row);

        let absorber_warning_label = gtk::Label::builder()
            .label("[!] DANGER: Waste ink absorber >= 80%! Ink Flush is hard-blocked to prevent overflow.")
            .css_classes(["error", "caption"])
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        absorber_group.add(&absorber_warning_label);

        status_box.append(&absorber_group);

        // FINE Printhead Health
        let head_group = adw::PreferencesGroup::builder()
            .title("FINE Printhead Cartridges")
            .build();

        let black_cartridge_row = adw::ActionRow::builder()
            .title("Black Printhead (BH-4)")
            .subtitle("OK (Installed)")
            .build();
        let color_cartridge_row = adw::ActionRow::builder()
            .title("Color Printhead (CH-41)")
            .subtitle("OK (Installed)")
            .build();
        head_group.add(&black_cartridge_row);
        head_group.add(&color_cartridge_row);
        status_box.append(&head_group);

        let btn_refresh = gtk::Button::builder()
            .label("Refresh Telemetry Now")
            .css_classes(["suggested-action", "pill"])
            .margin_top(8)
            .build();
        let s_ref = sender.clone();
        btn_refresh.connect_clicked(move |_| {
            s_ref.input(AppMsg::TriggerRefreshTelemetry);
        });
        status_box.append(&btn_refresh);

        let status_scroll = gtk::ScrolledWindow::builder()
            .child(&status_box)
            .vexpand(true)
            .build();

        let status_page = view_stack.add_titled(&status_scroll, Some("status"), "Status Monitor");
        status_page.set_icon_name(Some("dialog-information-symbolic"));

        main_box.append(&view_stack);
        toast_overlay.set_child(Some(&main_box));
        root.set_content(Some(&toast_overlay));

        let widgets = AppWidgets {
            print_summary_label,
            glossy_notice_label,
            flush_execute_btn,
            flush_confirm_switch,
            align_scales,
            pgbk_bar,
            pgbk_label,
            cyan_bar,
            cyan_label,
            magenta_bar,
            magenta_label,
            yellow_bar,
            yellow_label,
            absorber_bar,
            absorber_label,
            absorber_warning_label,
            black_cartridge_row,
            color_cartridge_row,
            diagnostic_banner_row,
            toast_overlay,
        };

        ComponentParts { model, widgets }
    }

    fn update(&mut self, message: Self::Input, sender: ComponentSender<Self>) {
        match message {
            AppMsg::MediaTypeChanged(idx) => {
                let media = match idx {
                    0 => MediaType::Plain,
                    1 => MediaType::PhotoPlusGlossyII,
                    2 => MediaType::PhotoProPlatinum,
                    3 => MediaType::PhotoProLuster,
                    4 => MediaType::PhotoPlusSemiGloss,
                    5 => MediaType::GlossyPhoto,
                    6 => MediaType::MattePhoto,
                    7 => MediaType::HighResolutionPaper,
                    8 => MediaType::Envelope,
                    9 => MediaType::Hagaki,
                    _ => MediaType::Cardstock,
                };
                self.vm.handle_event(StudioEvent::SetMediaType(media));
            }
            AppMsg::PrintQualityChanged(idx) => {
                let q = match idx {
                    0 => PrintQuality::Level1Fine,
                    1 => PrintQuality::Level2High,
                    2 => PrintQuality::Level3Standard,
                    3 => PrintQuality::Level4Fast,
                    _ => PrintQuality::Level5Draft,
                };
                self.vm.handle_event(StudioEvent::SetPrintQuality(q));
            }
            AppMsg::PaperSizeChanged(idx) => {
                let sz = match idx {
                    0 => PaperSize::A4,
                    1 => PaperSize::A5,
                    2 => PaperSize::A6,
                    3 => PaperSize::B5,
                    4 => PaperSize::Letter,
                    5 => PaperSize::Legal,
                    6 => PaperSize::Executive,
                    7 => PaperSize::Photo4x6,
                    8 => PaperSize::Photo5x7,
                    9 => PaperSize::Photo8x10,
                    10 => PaperSize::PhotoL,
                    11 => PaperSize::Square5x5,
                    12 => PaperSize::BusinessCard,
                    13 => PaperSize::EnvelopeCom10,
                    14 => PaperSize::EnvelopeDL,
                    _ => PaperSize::Hagaki,
                };
                self.vm.handle_event(StudioEvent::SetPaperSize(sz));
            }
            AppMsg::BorderlessChanged(idx) => {
                let b = match idx {
                    0 => BorderlessExtension::Off,
                    1 => BorderlessExtension::Extension0Min,
                    2 => BorderlessExtension::Extension1Low,
                    3 => BorderlessExtension::Extension2Med,
                    _ => BorderlessExtension::Extension3Max,
                };
                self.vm.handle_event(StudioEvent::SetBorderless(b));
            }
            AppMsg::PageLayoutChanged(idx) => {
                let l = match idx {
                    0 => PageLayout::Normal,
                    1 => PageLayout::FitToPage,
                    2 => PageLayout::Scaled { percent: 100 },
                    3 => PageLayout::NUp { pages_per_sheet: 2 },
                    4 => PageLayout::NUp { pages_per_sheet: 4 },
                    5 => PageLayout::NUp { pages_per_sheet: 6 },
                    6 => PageLayout::NUp { pages_per_sheet: 9 },
                    7 => PageLayout::NUp {
                        pages_per_sheet: 16,
                    },
                    8 => PageLayout::Poster { rows: 2, cols: 2 },
                    9 => PageLayout::Poster { rows: 3, cols: 3 },
                    _ => PageLayout::Booklet,
                };
                self.vm.handle_event(StudioEvent::SetPageLayout(l));
            }
            AppMsg::GrayscaleToggled(val) => {
                self.vm.handle_event(StudioEvent::SetGrayscale(val));
            }
            AppMsg::CyanBalanceChanged(val) => {
                self.vm
                    .handle_event(StudioEvent::SetCyanBalance(val.round() as i8));
            }
            AppMsg::MagentaBalanceChanged(val) => {
                self.vm
                    .handle_event(StudioEvent::SetMagentaBalance(val.round() as i8));
            }
            AppMsg::YellowBalanceChanged(val) => {
                self.vm
                    .handle_event(StudioEvent::SetYellowBalance(val.round() as i8));
            }
            AppMsg::BrightnessChanged(val) => {
                self.vm
                    .handle_event(StudioEvent::SetBrightness(val.round() as i8));
            }
            AppMsg::ContrastChanged(val) => {
                self.vm
                    .handle_event(StudioEvent::SetContrast(val.round() as i8));
            }
            AppMsg::PaperGapToggled(val) => {
                let gap = if val {
                    PaperGapMode::WidePreventAbrasion
                } else {
                    PaperGapMode::Auto
                };
                self.vm.handle_event(StudioEvent::SetPaperGap(gap));
            }
            AppMsg::DryingTimeChanged(idx) => {
                let d = match idx {
                    0 => InkDryingTime::Short,
                    1 => InkDryingTime::Normal,
                    2 => InkDryingTime::Long,
                    _ => InkDryingTime::ExtraLong,
                };
                self.vm.handle_event(StudioEvent::SetDryingTime(d));
            }
            AppMsg::QuietModeToggled(val) => {
                let q = if val {
                    QuietMode::AlwaysOn
                } else {
                    QuietMode::Off
                };
                self.vm.handle_event(StudioEvent::SetQuietMode(q));
            }

            // Scanner updates
            AppMsg::ScanResolutionChanged(idx) => {
                let r = match idx {
                    0 => ScanResolution::Dpi75,
                    1 => ScanResolution::Dpi100,
                    2 => ScanResolution::Dpi150,
                    3 => ScanResolution::Dpi200,
                    4 => ScanResolution::Dpi300,
                    5 => ScanResolution::Dpi400,
                    6 => ScanResolution::Dpi600,
                    _ => ScanResolution::Dpi1200,
                };
                self.vm.handle_event(StudioEvent::SetScanResolution(r));
            }
            AppMsg::ScanColorModeChanged(idx) => {
                let m = match idx {
                    0 => ScanColorMode::Color24Bit,
                    1 => ScanColorMode::Grayscale8Bit,
                    _ => ScanColorMode::Lineart1Bit,
                };
                self.vm.handle_event(StudioEvent::SetScanColorMode(m));
            }
            AppMsg::ScanAutoCropToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanAutoCrop(val));
            }
            AppMsg::ScanDeskewToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanDeskew(val));
            }
            AppMsg::ScanDescreenToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanDescreen(val));
            }
            AppMsg::ScanUnsharpToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanUnsharp(val));
            }
            AppMsg::ScanRemoveDustToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanRemoveDust(val));
            }
            AppMsg::ScanGutterShadowToggled(val) => {
                self.vm.handle_event(StudioEvent::SetScanGutterShadow(val));
            }
            AppMsg::ScanGammaChanged(val) => {
                self.vm.handle_event(StudioEvent::SetScanGamma(val as f32));
            }

            // Alignment
            AppMsg::AlignmentColumnChanged(col, val) => {
                self.vm
                    .handle_event(StudioEvent::SetAlignmentColumn(col, val));
            }

            // Ink Flush
            AppMsg::InkFlushConfirmationToggled(confirmed) => {
                self.vm
                    .handle_event(StudioEvent::SetInkFlushConfirmation(confirmed));
            }

            // Async Hardware Actions
            AppMsg::TriggerRefreshTelemetry => {
                if let Some(client_arc) = self.client.clone() {
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match guard.get_status().await {
                            Ok(status) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(format!(
                                    "Telemetry refreshed: {:?}",
                                    status.state
                                ))));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e.to_string())));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerPrintTestPage => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone.dispatch_print_test_page(&mut guard).await {
                            Ok(()) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(
                                    "Diagnostic test page submitted to printer.".into(),
                                )));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerClean(kind, target) => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone
                            .dispatch_cleaning(&mut guard, kind, target, false)
                            .await
                        {
                            Ok(st) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(format!(
                                    "Cleaning complete. Absorber: {:.1}%",
                                    st.waste_absorber_percent
                                ))));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerMechanicalClean(target) => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone.dispatch_mechanical_clean(&mut guard, target).await {
                            Ok(_) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(
                                    "Mechanical cleaning complete.".into(),
                                )));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerTestPrint(t) => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone.dispatch_test_print(&mut guard, t).await {
                            Ok(_) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(
                                    "Pattern print job submitted.".into(),
                                )));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerApplyAlignment => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone.dispatch_apply_alignment(&mut guard).await {
                            Ok(_) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(
                                    "Manual alignment offsets successfully saved to printer."
                                        .into(),
                                )));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }
            AppMsg::TriggerScan => {
                if let Some(client_arc) = self.client.clone() {
                    let mut vm_clone = self.vm.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let mut guard = client_arc.lock().await;
                        match vm_clone.dispatch_scan(&mut guard).await {
                            Ok(pages) => {
                                s.input(AppMsg::HardwareTaskFinished(Ok(format!(
                                    "Scan complete ({} page(s) acquired).",
                                    pages.len()
                                ))));
                            }
                            Err(e) => {
                                s.input(AppMsg::HardwareTaskFinished(Err(e)));
                            }
                        }
                    });
                }
            }

            AppMsg::HardwareTaskFinished(result) => {
                match result {
                    Ok(msg) => {
                        self.vm.status_banner = msg;
                        self.vm.last_error = None;
                    }
                    Err(err) => {
                        self.vm.last_error = Some(err.clone());
                        self.vm.status_banner = format!("[ERROR] {err}");
                    }
                }
                self.vm.is_busy = false;
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        // Tab 1 Print Settings
        widgets
            .print_summary_label
            .set_label(&self.vm.ui_print_summary());
        if self.vm.glossy_notification.is_some() {
            widgets.glossy_notice_label.set_visible(true);
        } else {
            widgets.glossy_notice_label.set_visible(false);
        }

        // Tab 2 Maintenance
        widgets
            .flush_execute_btn
            .set_sensitive(self.vm.ink_flush_confirmed && !self.vm.is_busy);
        widgets
            .flush_confirm_switch
            .set_active(self.vm.ink_flush_confirmed);

        // Tab 4 Status Monitor
        let pgbk = self.vm.device_status.ink_levels.pigment_black_percent;
        let cyan = self.vm.device_status.ink_levels.cyan_percent;
        let mag = self.vm.device_status.ink_levels.magenta_percent;
        let yel = self.vm.device_status.ink_levels.yellow_percent;

        widgets.pgbk_bar.set_value(pgbk as f64 / 100.0);
        widgets.pgbk_label.set_label(&format!("{pgbk}%"));
        widgets.cyan_bar.set_value(cyan as f64 / 100.0);
        widgets.cyan_label.set_label(&format!("{cyan}%"));
        widgets.magenta_bar.set_value(mag as f64 / 100.0);
        widgets.magenta_label.set_label(&format!("{mag}%"));
        widgets.yellow_bar.set_value(yel as f64 / 100.0);
        widgets.yellow_label.set_label(&format!("{yel}%"));

        let absorber = self.vm.device_status.waste_absorber_percent;
        widgets.absorber_bar.set_value(absorber as f64 / 100.0);
        widgets.absorber_label.set_label(&format!("{absorber:.1}%"));
        widgets.absorber_warning_label.set_visible(absorber >= 80.0);

        widgets
            .black_cartridge_row
            .set_subtitle(match self.vm.device_status.black_head_state {
                CartridgeState::InstalledOk => "OK (Installed)",
                CartridgeState::NotRecognized => "ERROR: Not Recognized",
                CartridgeState::NotInstalled => "MISSING: Not Installed",
                CartridgeState::LockUnlatched => "WARNING: Lever Unlatched",
                CartridgeState::Overheated => "CRITICAL: Overheated",
            });
        widgets
            .color_cartridge_row
            .set_subtitle(match self.vm.device_status.color_head_state {
                CartridgeState::InstalledOk => "OK (Installed)",
                CartridgeState::NotRecognized => "ERROR: Not Recognized",
                CartridgeState::NotInstalled => "MISSING: Not Installed",
                CartridgeState::LockUnlatched => "WARNING: Lever Unlatched",
                CartridgeState::Overheated => "CRITICAL: Overheated",
            });

        widgets
            .diagnostic_banner_row
            .set_title(&self.vm.status_banner);
    }
}
