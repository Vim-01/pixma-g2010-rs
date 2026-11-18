//! Native Wayland/X11 GTK4 + Libadwaita application for PIXMA G2010 Studio.

use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceCapabilities, RollerCleanTarget, TestPrintType,
};
use pixma_core::models::{CartridgeState, OperationalState};
use pixma_core::print_params::{
    BorderlessExtension, InkDryingTime, MediaType, PageLayout, PaperGapMode, PaperSize,
    PrintQuality, QuietMode,
};
use pixma_core::scan_params::{ScanColorMode, ScanResolution};
use pixma_daemon::{ConnectionSource, DaemonClient, JobLockKind, LiveDaemonSnapshot};
use relm4::adw::prelude::*;
use relm4::{adw, gtk, ComponentParts, ComponentSender, SimpleComponent};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::{Mutex as TokioMutex, Notify};

use crate::state::{
    ConnectionLedState, ConnectionTelemetry, StudioEvent, StudioViewModel,
    GLOSSY_COMPOSITE_CMY_NOTICE,
};

/// Startup configuration passed from `main.rs` into [`AppModel`].
pub struct AppLaunchInit {
    pub client: Option<DaemonClient>,
    pub is_mock_mode: bool,
    pub configured_socket: Option<String>,
    pub configured_remote_host: Option<String>,
}

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

    // Live telemetry updates from physical hardware / daemon
    LiveSnapshotReceived {
        source: ConnectionSource,
        snapshot: Box<LiveDaemonSnapshot>,
    },
    TelemetryDisconnected(String),
    MaintenanceCapabilitiesReceived(Option<MaintenanceCapabilities>),

    // Asynchronous hardware actions
    TriggerRefreshTelemetry,
    TriggerPrintTestPage,
    TriggerClean(CleaningType, CleaningTarget),
    TriggerMechanicalClean(RollerCleanTarget),
    TriggerTestPrint(TestPrintType),
    TriggerApplyAlignment,
    TriggerScan,
    ScanCompleted(
        Result<
            (
                usize,
                Option<std::path::PathBuf>,
                Option<std::path::PathBuf>,
            ),
            String,
        >,
    ),
    OpenScannedDocument,

    // Task completions
    HardwareTaskFinished(Result<String, String>),
}

/// Main application model holding the state view-model and hardware client.
pub struct AppModel {
    pub vm: StudioViewModel,
    pub client: Arc<TokioMutex<Option<DaemonClient>>>,
    pub poll_notify: Arc<Notify>,
    pub configured_socket: Option<PathBuf>,
    pub pending_toasts: RefCell<Vec<(String, u32)>>,
    pub last_maint_feedback: Option<Result<String, String>>,
    pub last_scan_error: Option<String>,
    pub capabilities_fetched: bool,
}

impl AppModel {
    fn notify_telemetry_refresh(&self, sender: &ComponentSender<Self>) {
        if self.vm.is_mock_mode {
            sender.input(AppMsg::TriggerRefreshTelemetry);
        } else {
            self.poll_notify.notify_one();
        }
    }

    /// Applies scan completion result, propagating concise errors and clearing/setting preview metadata.
    pub fn apply_scan_completed(
        &mut self,
        result: Result<(usize, Option<PathBuf>, Option<PathBuf>), String>,
    ) {
        self.vm.is_busy = false;
        self.vm.busy_operation = None;
        match result {
            Ok((count, path_opt, thumb_opt)) => {
                self.vm.last_scan_path = path_opt.clone();
                self.vm.last_scan_thumb = thumb_opt;
                self.last_scan_error = None;
                let path_str = path_opt
                    .as_ref()
                    .map(|p| format!("Сохранено в {}", p.display()))
                    .unwrap_or_else(|| format!("Получено страниц: {count}"));
                let msg = format!("Сканирование завершено. {path_str}");
                self.vm.status_banner = msg.clone();
                self.vm.last_error = None;
                self.pending_toasts.borrow_mut().push((msg, 5));
            }
            Err(err) => {
                let concise = format_concise_error(&err);
                self.vm.last_scan_path = None;
                self.vm.last_scan_thumb = None;
                self.vm.last_error = Some(concise.clone());
                self.last_scan_error = Some(concise.clone());
                self.vm.status_banner = format!("[ERROR] {concise}");
                self.pending_toasts
                    .borrow_mut()
                    .push((format_toast_error(&concise), 7));
            }
        }
    }

    /// Applies completion state for asynchronous hardware operations and resets
    /// the Ink Flush safety latch so every purge requires a fresh confirmation.
    pub fn apply_hardware_task_finished(&mut self, result: Result<String, String>) {
        self.vm
            .handle_event(StudioEvent::ResetInkFlushConfirmation);
        match &result {
            Ok(msg) => {
                self.vm.status_banner = msg.clone();
                self.vm.last_error = None;
                self.pending_toasts.borrow_mut().push((msg.clone(), 5));
            }
            Err(err) => {
                let concise = format_concise_error(err);
                self.vm.last_error = Some(concise.clone());
                self.vm.status_banner = format!("[ERROR] {concise}");
                self.pending_toasts
                    .borrow_mut()
                    .push((format_toast_error(&concise), 7));
            }
        }
        self.last_maint_feedback = Some(result);
        self.vm.is_busy = false;
        self.vm.busy_operation = None;
    }

    fn spawn_hardware_task<F>(
        &mut self,
        op_label: String,
        toast_msg: String,
        sender: &ComponentSender<Self>,
        op: F,
    ) where
        F: for<'a> FnOnce(&'a mut DaemonClient) -> ActionFutureFn<'a, String> + Send + 'static,
    {
        if self.vm.is_busy {
            return;
        }
        self.vm.is_busy = true;
        self.vm.last_error = None;
        self.vm.busy_operation = Some(op_label);
        self.pending_toasts.borrow_mut().push((toast_msg, 3));
        let client_arc = self.client.clone();
        let is_mock = self.vm.is_mock_mode;
        let configured_socket = self.configured_socket.clone();
        let s = sender.clone();
        tokio::spawn(async move {
            let res =
                run_action_with_client(client_arc, is_mock, configured_socket, op).await;
            s.input(AppMsg::HardwareTaskFinished(res));
        });
    }
}

/// Collection of widgets that need real-time updates when state changes.
pub struct AppWidgets {
    // Persistent connection status strip & LED
    pub led_drawing: gtk::DrawingArea,
    pub led_state_cell: Rc<Cell<ConnectionLedState>>,
    pub conn_headline_label: gtk::Label,
    pub conn_detail_label: gtk::Label,
    pub usb_badge_label: gtk::Label,

    // Connection & exact USB marker group (Tab 4)
    pub conn_mode_row: adw::ActionRow,
    pub usb_marker_row: adw::ActionRow,
    pub usb_endpoints_row: adw::ActionRow,
    pub ieee1284_row: adw::ActionRow,
    pub usb_port_ivec_row: adw::ActionRow,

    // Print settings
    pub print_summary_label: gtk::Label,
    pub glossy_notice_label: gtk::Label,
    pub print_test_btn: gtk::Button,

    // Maintenance
    pub maint_status_row: adw::ActionRow,
    pub maint_capabilities_row: adw::ActionRow,
    pub maint_spinner: gtk::Spinner,
    pub maint_action_buttons: Vec<gtk::Button>,
    pub flush_execute_buttons: Vec<gtk::Button>,
    pub flush_confirm_switch: gtk::Switch,
    pub align_scales: Vec<gtk::Scale>,

    // Status monitor
    pub ink_group: adw::PreferencesGroup,
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

    // ScanGear Pro
    pub scan_btn: gtk::Button,
    pub scan_spinner: gtk::Spinner,
    pub scan_preview_picture: gtk::Picture,
    pub scan_result_row: adw::ActionRow,
    pub scan_open_btn: gtk::Button,

    // Global
    pub toast_overlay: adw::ToastOverlay,
}

impl SimpleComponent for AppModel {
    type Input = AppMsg;
    type Output = ();
    type Init = AppLaunchInit;
    type Root = adw::ApplicationWindow;
    type Widgets = AppWidgets;

    fn init_root() -> Self::Root {
        adw::ApplicationWindow::builder()
            .title("PIXMA G2010 Studio")
            .default_width(940)
            .default_height(820)
            .build()
    }

    fn init(
        launch: Self::Init,
        root: Self::Root,
        sender: ComponentSender<Self>,
    ) -> ComponentParts<Self> {
        let mut vm = StudioViewModel::default();
        let configured_socket = launch.configured_socket.as_deref().map(PathBuf::from);
        vm.is_mock_mode = launch.is_mock_mode;
        vm.telemetry.configured_socket = launch.configured_socket;
        vm.telemetry.configured_remote_host = launch.configured_remote_host;

        let shared_client = Arc::new(TokioMutex::new(launch.client));
        let poll_notify = Arc::new(Notify::new());
        let model = Self {
            vm,
            client: Arc::clone(&shared_client),
            poll_notify: Arc::clone(&poll_notify),
            configured_socket: configured_socket.clone(),
            pending_toasts: RefCell::new(Vec::new()),
            last_maint_feedback: None,
            last_scan_error: None,
            capabilities_fetched: false,
        };

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

        // Persistent 4-color LED + USB Marker Connection Strip
        let conn_strip = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(10)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(14)
            .margin_end(14)
            .build();

        let led_state_cell = Rc::new(Cell::new(model.vm.led_state()));
        let led_cell_draw = Rc::clone(&led_state_cell);
        let led_drawing = gtk::DrawingArea::builder()
            .content_width(18)
            .content_height(18)
            .valign(gtk::Align::Center)
            .build();
        led_drawing.set_draw_func(move |_area, cr, width, height| {
            let state = led_cell_draw.get();
            let (r, g, b) = state.rgb();
            let cx = (width as f64) / 2.0;
            let cy = (height as f64) / 2.0;
            let radius = cx.min(cy) - 1.5;
            if radius <= 1.0 {
                return;
            }
            cr.set_source_rgba(r, g, b, 0.28);
            cr.arc(cx, cy, radius, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(r, g, b, 1.0);
            cr.arc(cx, cy, radius * 0.68, 0.0, std::f64::consts::TAU);
            let _ = cr.fill();
            cr.set_source_rgba(1.0, 1.0, 1.0, 0.45);
            cr.set_line_width(1.0);
            cr.arc(cx, cy, radius * 0.68, 0.0, std::f64::consts::TAU);
            let _ = cr.stroke();
        });
        conn_strip.append(&led_drawing);

        let conn_text_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
        conn_text_box.set_hexpand(true);
        let conn_headline_label = gtk::Label::builder()
            .label("Нет подключения")
            .css_classes(["heading"])
            .xalign(0.0)
            .build();
        let conn_detail_label = gtk::Label::builder()
            .label("Опрос физического устройства...")
            .css_classes(["caption", "dim-label"])
            .xalign(0.0)
            .ellipsize(gtk::pango::EllipsizeMode::End)
            .build();
        conn_text_box.append(&conn_headline_label);
        conn_text_box.append(&conn_detail_label);
        conn_strip.append(&conn_text_box);

        let usb_badge_label = gtk::Label::builder()
            .label("USB: ожидание")
            .css_classes(["caption", "monospace", "dim-label"])
            .valign(gtk::Align::Center)
            .build();
        conn_strip.append(&usb_badge_label);

        main_box.append(&conn_strip);
        main_box.append(&gtk::Separator::new(gtk::Orientation::Horizontal));

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
        // Pinned live Maintenance Operation Feedback Banner (stays visible when scrolling)
        let maint_status_group = adw::PreferencesGroup::builder()
            .title("Maintenance Status")
            .margin_top(12)
            .margin_bottom(8)
            .margin_start(16)
            .margin_end(16)
            .build();
        let maint_status_row = adw::ActionRow::builder()
            .title("Готово к обслуживанию")
            .subtitle("Ожидание команды")
            .subtitle_lines(0)
            .build();
        let maint_spinner = gtk::Spinner::new();
        maint_spinner.set_visible(false);
        maint_status_row.add_suffix(&maint_spinner);
        let maint_capabilities_row = adw::ActionRow::builder()
            .title("Поддерживаемые группы прошивки (GetCapability)")
            .subtitle("Ожидание подключения к принтеру...")
            .subtitle_lines(0)
            .build();
        maint_status_group.add(&maint_status_row);
        maint_status_group.add(&maint_capabilities_row);

        let maint_box = gtk::Box::new(gtk::Orientation::Vertical, 16);
        maint_box.set_margin_top(10);
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
            .subtitle("Select All, Black (group1), or Color (group2) if allowed by firmware")
            .build();
        let make_flush_btn = |label: &str, target: CleaningTarget| -> gtk::Button {
            let btn = gtk::Button::builder()
                .label(label)
                .css_classes(["destructive-action"])
                .sensitive(false)
                .build();
            let s_flush = sender.clone();
            btn.connect_clicked(move |_| {
                s_flush.input(AppMsg::TriggerClean(CleaningType::InkFlush, target));
            });
            btn
        };
        let btn_flush_all = make_flush_btn("All", CleaningTarget::All);
        let btn_flush_bk = make_flush_btn("Black", CleaningTarget::BlackOnly);
        let btn_flush_col = make_flush_btn("Color", CleaningTarget::ColorOnly);
        let flush_btn_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        flush_btn_box.append(&btn_flush_all);
        flush_btn_box.append(&btn_flush_bk);
        flush_btn_box.append(&btn_flush_col);
        flush_btn_row.add_suffix(&flush_btn_box);
        flush_group.add(&flush_btn_row);
        let flush_execute_buttons = vec![btn_flush_all, btn_flush_bk, btn_flush_col];

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

        let maint_action_buttons = vec![
            btn_reg_all,
            btn_reg_bk,
            btn_reg_col,
            btn_deep_all,
            btn_deep_bk,
            btn_deep_col,
            btn_roller,
            btn_platen,
            btn_nozzle,
            btn_man_align,
            btn_half_align,
            btn_apply_align,
        ];

        maint_box.append(&diag_group);

        let maint_scroll = gtk::ScrolledWindow::builder()
            .child(&maint_box)
            .vexpand(true)
            .build();

        let maint_tab_root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        maint_tab_root.append(&maint_status_group);
        maint_tab_root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        maint_tab_root.append(&maint_scroll);

        let maint_page = view_stack.add_titled(&maint_tab_root, Some("maintenance"), "Maintenance");
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

        let scan_action_box = gtk::Box::new(gtk::Orientation::Horizontal, 12);
        scan_action_box.set_margin_top(8);
        let btn_scan = gtk::Button::builder()
            .label("Acquire Scan Document & Export")
            .css_classes(["suggested-action", "pill"])
            .hexpand(true)
            .build();
        let scan_spinner = gtk::Spinner::new();

        let s_scan = sender.clone();
        btn_scan.connect_clicked(move |_| {
            s_scan.input(AppMsg::TriggerScan);
        });
        scan_action_box.append(&btn_scan);
        scan_action_box.append(&scan_spinner);
        scan_box.append(&scan_action_box);

        let scan_result_group = adw::PreferencesGroup::builder()
            .title("Acquired Scan Document Preview")
            .build();

        let scan_result_row = adw::ActionRow::builder()
            .title("No document acquired yet")
            .subtitle("Scans will be saved to ~/Pictures/Scans/ and previewed here")
            .build();

        let scan_open_btn = gtk::Button::builder()
            .label("Open in Viewer")
            .css_classes(["pill"])
            .visible(false)
            .build();
        let s_open = sender.clone();
        scan_open_btn.connect_clicked(move |_| {
            s_open.input(AppMsg::OpenScannedDocument);
        });
        scan_result_row.add_suffix(&scan_open_btn);
        scan_result_group.add(&scan_result_row);
        scan_box.append(&scan_result_group);

        let scan_preview_picture = gtk::Picture::builder()
            .can_shrink(true)
            .vexpand(true)
            .height_request(280)
            .visible(false)
            .build();
        scan_box.append(&scan_preview_picture);

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

        let conn_group = adw::PreferencesGroup::builder()
            .title("Подключение и точный USB-маркер")
            .description("Фактическое состояние физического канала, шины USB, порта IEEE-1284 и ответа IVEC")
            .build();

        let conn_mode_row = adw::ActionRow::builder()
            .title("Состояние и режим канала")
            .subtitle("Нет подключения (опрос...)")
            .build();
        let usb_marker_row = adw::ActionRow::builder()
            .title("Точный USB-маркер (Bus / Dev / VID:PID / S/N)")
            .subtitle("Нет подключения")
            .build();
        let usb_endpoints_row = adw::ActionRow::builder()
            .title("USB Интерфейсы и Endpoints")
            .subtitle("Нет данных")
            .build();
        let ieee1284_row = adw::ActionRow::builder()
            .title("IEEE-1284 GET_DEVICE_ID")
            .subtitle("Нет данных")
            .build();
        let usb_port_ivec_row = adw::ActionRow::builder()
            .title("USB Port Status + сырой ответ IVEC")
            .subtitle("Нет данных")
            .build();

        conn_group.add(&conn_mode_row);
        conn_group.add(&usb_marker_row);
        conn_group.add(&usb_endpoints_row);
        conn_group.add(&ieee1284_row);
        conn_group.add(&usb_port_ivec_row);
        status_box.append(&conn_group);

        let diag_banner_group = adw::PreferencesGroup::new();
        let diagnostic_banner_row = adw::ActionRow::builder()
            .title("Нет подключения: опрос устройства...")
            .subtitle("Ожидание реального ответа от принтера")
            .css_classes(["card"])
            .build();
        diag_banner_group.add(&diagnostic_banner_row);
        status_box.append(&diag_banner_group);

        // GI-790 CISS Ink Levels
        let ink_group = adw::PreferencesGroup::builder()
            .title("Уровни чернил GI-790 СНПЧ (ответ прошивки)")
            .description("Ожидание телеметрии от принтера")
            .build();

        let make_gauge_row = |name: &'static str| {
            let row = adw::ActionRow::builder().title(name).build();
            let box_h = gtk::Box::new(gtk::Orientation::Horizontal, 8);
            let bar = gtk::LevelBar::builder()
                .min_value(0.0)
                .max_value(1.0)
                .value(0.0)
                .width_request(180)
                .build();
            let lbl = gtk::Label::builder()
                .label("—")
                .width_request(95)
                .xalign(1.0)
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
            .title("Абсорбер отработанных чернил (D-Counter)")
            .description("Показания из ответа принтера (блокировка Ink Flush при >= 80%, ошибка 5B00/P07 при 100%)")
            .build();

        let absorber_row = adw::ActionRow::builder().title("Заполнение абсорбера").build();
        let abs_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        let absorber_bar = gtk::LevelBar::builder()
            .min_value(0.0)
            .max_value(1.0)
            .value(0.0)
            .width_request(180)
            .build();
        let absorber_label = gtk::Label::builder()
            .label("—")
            .width_request(110)
            .xalign(1.0)
            .build();
        abs_box.append(&absorber_bar);
        abs_box.append(&absorber_label);
        absorber_row.add_suffix(&abs_box);
        absorber_group.add(&absorber_row);

        let absorber_warning_label = gtk::Label::builder()
            .label("[!] ОПАСНОСТЬ: абсорбер >= 80%! Прокачка чернил (Ink Flush) заблокирована.")
            .css_classes(["error", "caption"])
            .wrap(true)
            .xalign(0.0)
            .visible(false)
            .build();
        absorber_group.add(&absorber_warning_label);

        status_box.append(&absorber_group);

        // FINE Printhead Health
        let head_group = adw::PreferencesGroup::builder()
            .title("Печатающие головки FINE")
            .build();

        let black_cartridge_row = adw::ActionRow::builder()
            .title("Чёрная головка (BH-4)")
            .subtitle("Неизвестно (нет подключения)")
            .build();
        let color_cartridge_row = adw::ActionRow::builder()
            .title("Цветная головка (CH-41)")
            .subtitle("Неизвестно (нет подключения)")
            .build();
        head_group.add(&black_cartridge_row);
        head_group.add(&color_cartridge_row);
        status_box.append(&head_group);

        let btn_refresh = gtk::Button::builder()
            .label("Обновить телеметрию сейчас")
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

        spawn_live_telemetry_poller(
            model.vm.is_mock_mode,
            model.configured_socket.clone(),
            Arc::clone(&shared_client),
            Arc::clone(&poll_notify),
            sender.clone(),
        );

        let widgets = AppWidgets {
            led_drawing,
            led_state_cell,
            conn_headline_label,
            conn_detail_label,
            usb_badge_label,
            conn_mode_row,
            usb_marker_row,
            usb_endpoints_row,
            ieee1284_row,
            usb_port_ivec_row,
            print_summary_label,
            glossy_notice_label,
            print_test_btn,
            maint_status_row,
            maint_capabilities_row,
            maint_spinner,
            maint_action_buttons,
            flush_execute_buttons,
            flush_confirm_switch,
            align_scales,
            ink_group,
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
            scan_btn: btn_scan,
            scan_spinner,
            scan_preview_picture,
            scan_result_row,
            scan_open_btn,
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

            // Live telemetry events
            AppMsg::LiveSnapshotReceived { source, snapshot } => {
                self.vm
                    .handle_event(StudioEvent::ApplyLiveSnapshot { source, snapshot });
                if !self.capabilities_fetched {
                    self.capabilities_fetched = true;
                    let client_arc = self.client.clone();
                    let is_mock = self.vm.is_mock_mode;
                    let configured_socket = self.configured_socket.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let res = run_action_with_client(
                            client_arc,
                            is_mock,
                            configured_socket,
                            |c| {
                                Box::pin(async move {
                                    // Always return Ok(...) to avoid tearing down a healthy DirectUsb handle
                                    Ok(c.get_maintenance_capabilities()
                                        .await
                                        .map_err(|e| e.to_string()))
                                })
                            },
                        )
                        .await;
                        let caps_opt = res.ok().and_then(|inner| inner.ok());
                        s.input(AppMsg::MaintenanceCapabilitiesReceived(caps_opt));
                    });
                }
            }
            AppMsg::TelemetryDisconnected(err) => {
                self.capabilities_fetched = false;
                self.vm
                    .handle_event(StudioEvent::ReportTelemetryDisconnected(err));
            }
            AppMsg::MaintenanceCapabilitiesReceived(caps) => {
                self.vm
                    .handle_event(StudioEvent::SetMaintenanceCapabilities(caps));
            }

            // Asynchronous hardware actions
            AppMsg::TriggerRefreshTelemetry => {
                if self.vm.is_mock_mode {
                    let client_arc = self.client.clone();
                    let configured_socket = self.configured_socket.clone();
                    let s = sender.clone();
                    tokio::spawn(async move {
                        let res = run_action_with_client(
                            client_arc,
                            true,
                            configured_socket,
                            |c| {
                                Box::pin(async move {
                                    let snap =
                                        c.get_live_snapshot().await.map_err(|e| e.to_string())?;
                                    Ok((c.source().clone(), Box::new(snap)))
                                })
                            },
                        )
                        .await;
                        match res {
                            Ok((source, snapshot)) => {
                                s.input(AppMsg::LiveSnapshotReceived { source, snapshot });
                            }
                            Err(e) => {
                                s.input(AppMsg::TelemetryDisconnected(e));
                            }
                        }
                    });
                } else {
                    self.poll_notify.notify_one();
                }
            }
            AppMsg::TriggerPrintTestPage => {
                if self.vm.is_busy {
                    return;
                }
                let mut vm_clone = self.vm.clone();
                self.spawn_hardware_task(
                    "Печать тестовой страницы".to_string(),
                    "Отправка тестовой страницы на принтер...".to_string(),
                    &sender,
                    |c| {
                        Box::pin(async move {
                            vm_clone.dispatch_print_test_page(c).await?;
                            Ok("Тестовая страница отправлена на принтер.".to_string())
                        })
                    },
                );
            }
            AppMsg::TriggerClean(kind, target) => {
                if self.vm.is_busy {
                    return;
                }
                let op_label = format!("Очистка ({kind:?} · {target:?})");
                let mut vm_clone = self.vm.clone();
                self.spawn_hardware_task(
                    op_label.clone(),
                    format!("Запуск: {op_label}..."),
                    &sender,
                    move |c| {
                        Box::pin(async move {
                            let st = vm_clone.dispatch_cleaning(c, kind, target, false).await?;
                            Ok(format!(
                                "Прочистка принята принтером. Текущий статус: {:?}",
                                st.state
                            ))
                        })
                    },
                );
            }
            AppMsg::TriggerMechanicalClean(target) => {
                if self.vm.is_busy {
                    return;
                }
                let mut vm_clone = self.vm.clone();
                self.spawn_hardware_task(
                    "Очистка роликов / поддона".to_string(),
                    "Запуск механической очистки...".to_string(),
                    &sender,
                    move |c| {
                        Box::pin(async move {
                            let st = vm_clone.dispatch_mechanical_clean(c, target).await?;
                            Ok(format!(
                                "Механическая очистка запущена. Статус: {:?}",
                                st.state
                            ))
                        })
                    },
                );
            }
            AppMsg::TriggerTestPrint(t) => {
                if self.vm.is_busy {
                    return;
                }
                let mut vm_clone = self.vm.clone();
                self.spawn_hardware_task(
                    format!("Тест-шаблон ({t:?})"),
                    format!("Отправка шаблона {t:?}..."),
                    &sender,
                    move |c| {
                        Box::pin(async move {
                            let st = vm_clone.dispatch_test_print(c, t).await?;
                            Ok(format!(
                                "Задание диагностической печати принято. Статус: {:?}",
                                st.state
                            ))
                        })
                    },
                );
            }
            AppMsg::TriggerApplyAlignment => {
                if self.vm.is_busy {
                    return;
                }
                let mut vm_clone = self.vm.clone();
                self.spawn_hardware_task(
                    "Запись калибровки головок".to_string(),
                    "Запись смещений A..N в NVRAM...".to_string(),
                    &sender,
                    |c| {
                        Box::pin(async move {
                            vm_clone.dispatch_apply_alignment(c).await?;
                            Ok("Значения выравнивания сохранены в принтер.".to_string())
                        })
                    },
                );
            }
            AppMsg::TriggerScan => {
                if self.vm.is_busy {
                    return;
                }
                self.vm.is_busy = true;
                self.vm.last_error = None;
                self.last_scan_error = None;
                self.vm.busy_operation = Some("Сканирование планшета MP2".to_string());
                let client_arc = self.client.clone();
                let mut vm_clone = self.vm.clone();
                let is_mock = self.vm.is_mock_mode;
                let configured_socket = self.configured_socket.clone();
                let s = sender.clone();
                tokio::spawn(async move {
                    let res =
                        run_action_with_client(client_arc, is_mock, configured_socket, |c| {
                            Box::pin(async move {
                                let pages = vm_clone.dispatch_scan(c).await?;
                                Ok((
                                    pages.len(),
                                    vm_clone.last_scan_path,
                                    vm_clone.last_scan_thumb,
                                ))
                            })
                        })
                        .await;

                    s.input(AppMsg::ScanCompleted(res));
                });
            }

            AppMsg::ScanCompleted(result) => {
                self.apply_scan_completed(result);
                self.notify_telemetry_refresh(&sender);
            }

            AppMsg::OpenScannedDocument => {
                if let Some(path) = &self.vm.last_scan_path {
                    if let Err(err) = std::process::Command::new("xdg-open").arg(path).spawn() {
                        let msg =
                            format!("Не удалось открыть скан ({}): {err}", path.display());
                        self.vm.last_error = Some(msg.clone());
                        self.pending_toasts.borrow_mut().push((msg, 5));
                    }
                }
            }

            AppMsg::HardwareTaskFinished(result) => {
                self.apply_hardware_task_finished(result);
                self.notify_telemetry_refresh(&sender);
            }
        }
    }

    fn update_view(&self, widgets: &mut Self::Widgets, _sender: ComponentSender<Self>) {
        for (text, timeout) in self.pending_toasts.borrow_mut().drain(..) {
            let toast = adw::Toast::builder()
                .title(&text)
                .timeout(timeout)
                .build();
            widgets.toast_overlay.add_toast(toast);
        }

        // Persistent 4-color LED + exact USB marker strip
        let led = self.vm.led_state();
        widgets.led_state_cell.set(led);
        widgets.led_drawing.queue_draw();

        let (headline, detail) = format_connection_strip(&self.vm, led);
        widgets.conn_headline_label.set_label(&headline);
        widgets.conn_detail_label.set_label(&detail);
        widgets
            .usb_badge_label
            .set_label(&self.vm.telemetry.usb_marker_short());

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
        let hw_active_state = self
            .vm
            .telemetry
            .snapshot
            .as_ref()
            .filter(|_| self.vm.telemetry.connected)
            .and_then(|snap| {
                matches!(
                    snap.status.state,
                    OperationalState::Cleaning
                        | OperationalState::Testing
                        | OperationalState::Printing
                        | OperationalState::Scanning
                        | OperationalState::Waiting
                )
                .then_some(snap.status.state)
            });
        let hw_busy = hw_active_state.is_some();
        let maint_locked = self.vm.is_busy || hw_busy;

        widgets.print_test_btn.set_sensitive(!maint_locked);
        for btn in &widgets.maint_action_buttons {
            btn.set_sensitive(!maint_locked);
        }
        for btn in &widgets.flush_execute_buttons {
            btn.set_sensitive(self.vm.ink_flush_confirmed && !maint_locked);
        }
        widgets
            .flush_confirm_switch
            .set_active(self.vm.ink_flush_confirmed);

        let caps_sub = match self.vm.maintenance_capabilities.as_ref() {
            Some(caps) => caps
                .format_summary()
                .map(|s| format!("Прошивка разрешает: {s}"))
                .unwrap_or_else(|| {
                    "GetCapabilityResponse получен без тегов <ivec:Cleaning>; все группы доступны"
                        .to_string()
                }),
            None if self.vm.telemetry.connected => {
                "GetCapability не возвращён (возможно pixmad < 0.3.1); ограничения не применяются"
                    .to_string()
            }
            None => "Ожидание подключения к принтеру...".to_string(),
        };
        set_row_subtitle_plain(&widgets.maint_capabilities_row, &caps_sub);

        if self.vm.is_busy {
            widgets.maint_spinner.set_visible(true);
            widgets.maint_spinner.start();
            set_row_title_plain(&widgets.maint_status_row, "Выполняется команда...");
            set_row_subtitle_plain(
                &widgets.maint_status_row,
                self.vm
                    .busy_operation
                    .as_deref()
                    .unwrap_or("Отправка команды на принтер..."),
            );
        } else if let Some(active_state) = hw_active_state {
            widgets.maint_spinner.set_visible(true);
            widgets.maint_spinner.start();
            set_row_title_plain(
                &widgets.maint_status_row,
                "Принтер выполняет обслуживание",
            );
            set_row_subtitle_plain(
                &widgets.maint_status_row,
                &format!(
                    "Реальный ответ принтера: {} ({:?})",
                    format_op_state_ru(active_state),
                    active_state
                ),
            );
        } else if let Some(Err(ref err)) = self.last_maint_feedback {
            widgets.maint_spinner.stop();
            widgets.maint_spinner.set_visible(false);
            set_row_title_plain(&widgets.maint_status_row, "Ошибка / отклонено");
            set_row_subtitle_plain(&widgets.maint_status_row, &format_concise_error(err));
        } else if let Some(Ok(ref msg)) = self.last_maint_feedback {
            widgets.maint_spinner.stop();
            widgets.maint_spinner.set_visible(false);
            set_row_title_plain(&widgets.maint_status_row, "Последняя операция");
            set_row_subtitle_plain(
                &widgets.maint_status_row,
                &format!("{msg} · Текущий статус: {}", self.vm.status_banner),
            );
        } else {
            widgets.maint_spinner.stop();
            widgets.maint_spinner.set_visible(false);
            set_row_title_plain(&widgets.maint_status_row, "Готово к обслуживанию");
            set_row_subtitle_plain(&widgets.maint_status_row, &self.vm.status_banner);
        }

        // Tab 4 Status Monitor: Connection + USB Hardware Marker rows
        update_connection_detail_rows(widgets, &self.vm);

        // Tab 4 Status Monitor: Real hardware telemetry (no fake values when disconnected or unreported)
        match self.vm.telemetry.snapshot.as_ref() {
            Some(snap) if self.vm.telemetry.connected => {
                if snap.firmware_reported_ink {
                    widgets.ink_group.set_description(Some(
                        "Показания получены из IVEC-ответа прошивки принтера",
                    ));
                    let pgbk = snap.status.ink_levels.pigment_black_percent;
                    let cyan = snap.status.ink_levels.cyan_percent;
                    let mag = snap.status.ink_levels.magenta_percent;
                    let yel = snap.status.ink_levels.yellow_percent;

                    widgets.pgbk_bar.set_value(pgbk as f64 / 100.0);
                    widgets.pgbk_label.set_label(&format!("{pgbk}%"));
                    widgets.cyan_bar.set_value(cyan as f64 / 100.0);
                    widgets.cyan_label.set_label(&format!("{cyan}%"));
                    widgets.magenta_bar.set_value(mag as f64 / 100.0);
                    widgets.magenta_label.set_label(&format!("{mag}%"));
                    widgets.yellow_bar.set_value(yel as f64 / 100.0);
                    widgets.yellow_label.set_label(&format!("{yel}%"));
                } else {
                    widgets.ink_group.set_description(Some(
                        "Прошивка принтера в GetStatusResponse не передаёт % чернил — физические датчики уровня в СНПЧ G2010 отсутствуют",
                    ));
                    widgets.pgbk_bar.set_value(0.0);
                    widgets.pgbk_label.set_label("Нет датчика");
                    widgets.cyan_bar.set_value(0.0);
                    widgets.cyan_label.set_label("Нет датчика");
                    widgets.magenta_bar.set_value(0.0);
                    widgets.magenta_label.set_label("Нет датчика");
                    widgets.yellow_bar.set_value(0.0);
                    widgets.yellow_label.set_label("Нет датчика");
                }

                if snap.firmware_reported_waste {
                    let absorber = snap.status.waste_absorber_percent;
                    widgets.absorber_bar.set_value(absorber as f64 / 100.0);
                    widgets.absorber_label.set_label(&format!("{absorber:.1}%"));
                    widgets.absorber_warning_label.set_visible(absorber >= 80.0);
                } else {
                    widgets.absorber_bar.set_value(0.0);
                    widgets.absorber_label.set_label("Не передано");
                    widgets.absorber_warning_label.set_visible(false);
                }

                widgets.black_cartridge_row.set_subtitle(
                    format_cartridge_state_ru(snap.status.black_head_state),
                );
                widgets.color_cartridge_row.set_subtitle(
                    format_cartridge_state_ru(snap.status.color_head_state),
                );
            }
            _ => {
                widgets.ink_group.set_description(Some(
                    "Нет подключения к принтеру — данные о чернилах недоступны",
                ));
                widgets.pgbk_bar.set_value(0.0);
                widgets.pgbk_label.set_label("—");
                widgets.cyan_bar.set_value(0.0);
                widgets.cyan_label.set_label("—");
                widgets.magenta_bar.set_value(0.0);
                widgets.magenta_label.set_label("—");
                widgets.yellow_bar.set_value(0.0);
                widgets.yellow_label.set_label("—");

                widgets.absorber_bar.set_value(0.0);
                widgets.absorber_label.set_label("—");
                widgets.absorber_warning_label.set_visible(false);

                widgets
                    .black_cartridge_row
                    .set_subtitle("Неизвестно (нет подключения)");
                widgets
                    .color_cartridge_row
                    .set_subtitle("Неизвестно (нет подключения)");
            }
        }

        set_row_title_plain(&widgets.diagnostic_banner_row, &self.vm.status_banner);
        let banner_sub = match (&self.vm.telemetry.snapshot, &self.vm.telemetry.last_error) {
            (_, Some(err)) => format!("Ошибка связи: {err}"),
            (Some(snap), None) => match (&snap.status.support_code, &snap.status.lcd_code) {
                (Some(code), Some(lcd)) => format!("Активен код поддержки {code} (LCD: {lcd})"),
                (Some(code), None) => format!("Активен код поддержки {code}"),
                (None, _) => "Активных кодов ошибок от принтера нет".to_string(),
            },
            (None, None) => "Ожидание реального ответа от принтера".to_string(),
        };
        set_row_subtitle_plain(&widgets.diagnostic_banner_row, &banner_sub);

        // Tab 3 ScanGear Pro
        widgets.scan_btn.set_sensitive(!maint_locked);
        if self.vm.is_busy {
            widgets.scan_spinner.start();
        } else {
            widgets.scan_spinner.stop();
        }
        if self.last_scan_error.is_some() {
            widgets.scan_preview_picture.set_filename(None::<&std::path::Path>);
            widgets.scan_preview_picture.set_visible(false);
        } else if let Some(thumb) = &self.vm.last_scan_thumb {
            widgets.scan_preview_picture.set_filename(Some(thumb));
            widgets.scan_preview_picture.set_visible(true);
        } else if let Some(path) = &self.vm.last_scan_path {
            widgets.scan_preview_picture.set_filename(Some(path));
            widgets.scan_preview_picture.set_visible(true);
        } else {
            widgets.scan_preview_picture.set_filename(None::<&std::path::Path>);
            widgets.scan_preview_picture.set_visible(false);
        }
        if let Some(ref scan_err) = self.last_scan_error {
            set_row_title_plain(&widgets.scan_result_row, "Ошибка сканирования");
            set_row_subtitle_plain(&widgets.scan_result_row, scan_err);
            widgets.scan_open_btn.set_visible(false);
        } else if let Some(path) = &self.vm.last_scan_path {
            set_row_title_plain(&widgets.scan_result_row, "Scan Acquired & Exported Successfully");
            set_row_subtitle_plain(&widgets.scan_result_row, &path.display().to_string());
            widgets.scan_open_btn.set_visible(true);
        }
    }
}

enum PollOutcome {
    Snapshot {
        source: ConnectionSource,
        snapshot: Box<LiveDaemonSnapshot>,
    },
    DirectBusy,
    Disconnected(String),
}

type ActionFutureFn<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, String>> + Send + 'a>>;

fn spawn_live_telemetry_poller(
    is_mock_mode: bool,
    configured_socket: Option<PathBuf>,
    shared_direct: Arc<TokioMutex<Option<DaemonClient>>>,
    poll_notify: Arc<Notify>,
    sender: ComponentSender<AppModel>,
) {
    if is_mock_mode {
        sender.input(AppMsg::TriggerRefreshTelemetry);
        return;
    }

    tokio::spawn(async move {
        let mut socket_poll_client: Option<DaemonClient> = None;

        loop {
            let outcome = poll_once(
                &mut socket_poll_client,
                &shared_direct,
                configured_socket.as_deref(),
            )
            .await;
            let connected_now = match outcome {
                PollOutcome::Snapshot { source, snapshot } => {
                    sender.input(AppMsg::LiveSnapshotReceived { source, snapshot });
                    true
                }
                PollOutcome::DirectBusy => true,
                PollOutcome::Disconnected(err) => {
                    sender.input(AppMsg::TelemetryDisconnected(err));
                    false
                }
            };

            let delay = if connected_now {
                std::time::Duration::from_millis(1500)
            } else {
                std::time::Duration::from_millis(2500)
            };

            tokio::select! {
                _ = tokio::time::sleep(delay) => {}
                _ = poll_notify.notified() => {}
            }
        }
    });
}

async fn poll_once(
    socket_poll_client: &mut Option<DaemonClient>,
    shared_direct: &Arc<TokioMutex<Option<DaemonClient>>>,
    configured_socket: Option<&Path>,
) -> PollOutcome {
    if let Some(ref mut client) = socket_poll_client {
        return match tokio::time::timeout(
            std::time::Duration::from_secs(4),
            client.get_live_snapshot(),
        )
        .await
        {
            Ok(Ok(snapshot)) => PollOutcome::Snapshot {
                source: client.source().clone(),
                snapshot: Box::new(snapshot),
            },
            Ok(Err(e)) => {
                *socket_poll_client = None;
                PollOutcome::Disconnected(e.to_string())
            }
            Err(_) => {
                *socket_poll_client = None;
                PollOutcome::Disconnected("Таймаут опроса сокета демона (4с)".to_string())
            }
        };
    }

    let Ok(mut guard) = shared_direct.try_lock() else {
        return PollOutcome::DirectBusy;
    };

    if let Some(ref mut direct_client) = *guard {
        return match tokio::time::timeout(
            std::time::Duration::from_secs(4),
            direct_client.get_live_snapshot(),
        )
        .await
        {
            Ok(Ok(snapshot)) => PollOutcome::Snapshot {
                source: direct_client.source().clone(),
                snapshot: Box::new(snapshot),
            },
            Ok(Err(e)) => {
                *guard = None;
                PollOutcome::Disconnected(e.to_string())
            }
            Err(_) => {
                *guard = None;
                PollOutcome::Disconnected("Таймаут опроса USB-устройства (4с)".to_string())
            }
        };
    }

    let connect_res = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        DaemonClient::connect_live(configured_socket, None),
    )
    .await;

    let mut new_client = match connect_res {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => return PollOutcome::Disconnected(e.to_string()),
        Err(_) => {
            return PollOutcome::Disconnected("Таймаут подключения к принтеру (3с)".to_string())
        }
    };

    let snap_res = tokio::time::timeout(
        std::time::Duration::from_secs(4),
        new_client.get_live_snapshot(),
    )
    .await;

    match snap_res {
        Ok(Ok(snapshot)) => {
            let source = new_client.source().clone();
            if matches!(source, ConnectionSource::DaemonSocket { .. }) {
                *socket_poll_client = Some(new_client);
            } else {
                *guard = Some(new_client);
            }
            PollOutcome::Snapshot {
                source,
                snapshot: Box::new(snapshot),
            }
        }
        Ok(Err(e)) => PollOutcome::Disconnected(e.to_string()),
        Err(_) => PollOutcome::Disconnected("Таймаут получения ответа GetStatus (4с)".to_string()),
    }
}

async fn run_action_with_client<F, T>(
    shared_direct: Arc<TokioMutex<Option<DaemonClient>>>,
    is_mock_mode: bool,
    configured_socket: Option<PathBuf>,
    op: F,
) -> Result<T, String>
where
    F: for<'a> FnOnce(&'a mut DaemonClient) -> ActionFutureFn<'a, T>,
{
    let action_socket = {
        let mut guard = shared_direct.lock().await;
        if is_mock_mode {
            if guard.is_none() {
                *guard = Some(DaemonClient::connect_mock(None).map_err(|e| e.to_string())?);
            }
            let client = guard
                .as_mut()
                .ok_or_else(|| "Mock client unavailable after initialization".to_string())?;
            return op(client).await;
        }

        if let Some(ref mut existing_direct) = *guard {
            let res = op(existing_direct).await;
            if res.is_err() {
                *guard = None;
            }
            return res;
        }

        let new_client = DaemonClient::connect_live(configured_socket.as_deref(), None)
            .await
            .map_err(|e| e.to_string())?;
        if matches!(new_client.source(), ConnectionSource::DaemonSocket { .. }) {
            Some(new_client)
        } else {
            *guard = Some(new_client);
            let direct_client = guard
                .as_mut()
                .ok_or_else(|| "Direct USB client unavailable after initialization".to_string())?;
            let res = op(direct_client).await;
            if res.is_err() {
                *guard = None;
            }
            return res;
        }
    };

    let mut c =
        action_socket.ok_or_else(|| "Daemon socket client unexpectedly missing".to_string())?;
    op(&mut c).await
}

fn format_connection_strip(vm: &StudioViewModel, led: ConnectionLedState) -> (String, String) {
    if let Some(snap) = vm.telemetry.snapshot.as_ref().filter(|_| vm.telemetry.connected) {
        let model_name = snap.hardware_marker.product_name.as_deref().unwrap_or("Canon PIXMA");
        let transport_scope = if vm.telemetry.is_remote() {
            format!("Удалённо: {} ← {}", snap.daemon_hostname, vm.telemetry.local_hostname)
        } else if snap.hardware_marker.is_mock {
            "Локальный мок-эмулятор (--mock)".to_string()
        } else {
            format!("Локальная USB-шина: {}", snap.daemon_hostname)
        };

        let headline = match led {
            ConnectionLedState::Disconnected => format!("Нет подключения · {transport_scope}"),
            ConnectionLedState::LocalUsbReady => format!("На USB-шине (локально) · {model_name}"),
            ConnectionLedState::RemoteReady => {
                format!("Удалённо вкл ({}) · {model_name}", snap.daemon_hostname)
            }
            ConnectionLedState::Busy => {
                let reason = if let Some(ref op) = vm.busy_operation {
                    op.as_str()
                } else if let Some(job) = snap.active_job {
                    format_job_kind_ru(job)
                } else {
                    format_op_state_ru(snap.status.state)
                };
                format!("Печатает / Занят: {reason}")
            }
        };

        let ivec_status = snap.raw_ivec_status.as_deref().unwrap_or("—");
        let ivec_resp = snap.ivec_response.as_deref().unwrap_or("—");
        let detail = format!("{transport_scope} · IVEC {ivec_resp}/{ivec_status}");
        (headline, detail)
    } else {
        let err = vm
            .telemetry
            .last_error
            .as_deref()
            .unwrap_or("ожидание ответа от принтера / демона...");
        let target_hint = match (
            &vm.telemetry.configured_remote_host,
            &vm.telemetry.configured_socket,
        ) {
            (Some(host), Some(sock)) => format!("Удалённый канал {host} ({sock}): {err}"),
            (None, Some(sock)) => format!("Сокет {sock}: {err}"),
            _ => err.to_string(),
        };
        ("Нет подключения".to_string(), target_hint)
    }
}

fn set_row_title_plain(row: &adw::ActionRow, text: &str) {
    let escaped = gtk::glib::markup_escape_text(text);
    row.set_title(&escaped);
}

fn set_row_subtitle_plain(row: &adw::ActionRow, text: &str) {
    let escaped = gtk::glib::markup_escape_text(text);
    row.set_subtitle(&escaped);
}

fn update_connection_detail_rows(widgets: &AppWidgets, vm: &StudioViewModel) {
    if let Some(snap) = vm.telemetry.snapshot.as_ref().filter(|_| vm.telemetry.connected) {
        let mode_str = match &vm.telemetry.source {
            Some(ConnectionSource::DaemonSocket { socket_path }) => {
                if vm.telemetry.is_remote() {
                    format!(
                        "Удалённо по SSH-мосту · клиент {} → хост принтера {} · сокет {}",
                        vm.telemetry.local_hostname, snap.daemon_hostname, socket_path
                    )
                } else {
                    format!(
                        "Локальный демон pixmad ({}) · сокет {}",
                        snap.daemon_hostname, socket_path
                    )
                }
            }
            Some(ConnectionSource::DirectUsb) => format!(
                "Локальная USB-шина напрямую (in-process nusb) на {}",
                vm.telemetry.local_hostname
            ),
            Some(ConnectionSource::DirectMock) | None => {
                "Виртуальный эмулятор (--mock), реальная шина USB не используется".to_string()
            }
        };
        set_row_subtitle_plain(&widgets.conn_mode_row, &mode_str);

        let m = &snap.hardware_marker;
        let marker_full = if m.is_mock {
            format!("MOCK · S/N {} (физический USB не подключён)", m.serial_number)
        } else {
            let bus = m
                .bus_number
                .map(|b| format!("{b:03}"))
                .unwrap_or_else(|| "?".into());
            let dev = m
                .device_address
                .map(|d| format!("{d:03}"))
                .unwrap_or_else(|| "?".into());
            let vid = m
                .vendor_id
                .map(|v| format!("{v:04x}"))
                .unwrap_or_else(|| "????".into());
            let pid = m
                .product_id
                .map(|p| format!("{p:04x}"))
                .unwrap_or_else(|| "????".into());
            let mfg = m.manufacturer.as_deref().unwrap_or("?");
            let prod = m.product_name.as_deref().unwrap_or("?");
            format!(
                "Bus {bus} Dev {dev} | VID:PID {vid}:{pid} | {mfg} {prod} | S/N {}",
                m.serial_number
            )
        };
        set_row_subtitle_plain(&widgets.usb_marker_row, &marker_full);
        set_row_subtitle_plain(&widgets.usb_endpoints_row, &m.endpoints_summary);
        set_row_subtitle_plain(
            &widgets.ieee1284_row,
            snap.ieee1284_id.as_deref().unwrap_or("Не передано устройством"),
        );

        let port_str = snap
            .usb_port_status
            .map(ConnectionTelemetry::format_port_status)
            .unwrap_or_else(|| "Не получен".to_string());
        let fw_str = if snap.firmware_reported_version && !snap.status.firmware_version.is_empty()
        {
            snap.status.firmware_version.as_str()
        } else {
            "не передана прошивкой"
        };
        let active_str = match snap.active_job {
            Some(kind) => format!(" · Арбитр USB: занят ({})", format_job_kind_ru(kind)),
            None if vm.is_busy => " · Арбитр UI: выполняется операция".to_string(),
            None => String::new(),
        };
        let ivec_str = format!(
            "{} | IVEC: resp={}, status={}, detail={}, support={}, FW={}{}",
            port_str,
            snap.ivec_response.as_deref().unwrap_or("—"),
            snap.raw_ivec_status.as_deref().unwrap_or("—"),
            snap.raw_status_detail.as_deref().unwrap_or("none"),
            snap.raw_support_code.as_deref().unwrap_or("none"),
            fw_str,
            active_str,
        );
        set_row_subtitle_plain(&widgets.usb_port_ivec_row, &ivec_str);
    } else {
        let err = vm
            .telemetry
            .last_error
            .as_deref()
            .unwrap_or("ожидание ответа...");
        let remote_prefix = vm
            .telemetry
            .configured_remote_host
            .as_deref()
            .map(|h| format!("Удалённый канал ({h}) не активен: "))
            .unwrap_or_default();
        set_row_subtitle_plain(&widgets.conn_mode_row, &format!("{remote_prefix}{err}"));
        set_row_subtitle_plain(
            &widgets.usb_marker_row,
            "Физическое USB-устройство не обнаружено / не отвечает",
        );
        set_row_subtitle_plain(&widgets.usb_endpoints_row, "Нет данных");
        set_row_subtitle_plain(&widgets.ieee1284_row, "Нет данных");
        set_row_subtitle_plain(&widgets.usb_port_ivec_row, "Нет данных");
    }
}

fn format_cartridge_state_ru(state: CartridgeState) -> &'static str {
    match state {
        CartridgeState::InstalledOk => "OK (ошибка картриджа от прошивки не активна)",
        CartridgeState::NotRecognized => "ОШИБКА: не распознана (код ошибки)",
        CartridgeState::NotInstalled => "ОТСУТСТВУЕТ: не установлена",
        CartridgeState::LockUnlatched => "ВНИМАНИЕ: фиксатор не защёлкнут",
        CartridgeState::Overheated => "КРИТИЧНО: перегрев (5200)",
    }
}

fn format_job_kind_ru(kind: JobLockKind) -> &'static str {
    match kind {
        JobLockKind::Printing => "Печать (поток USB)",
        JobLockKind::Scanning => "Сканирование (MP2)",
        JobLockKind::Maintenance => "Обслуживание",
        JobLockKind::Telemetry => "Опрос телеметрии",
    }
}

fn format_op_state_ru(state: OperationalState) -> &'static str {
    match state {
        OperationalState::Idle => "Готов",
        OperationalState::Printing => "Печать",
        OperationalState::Scanning => "Сканирование",
        OperationalState::Cleaning => "Очистка",
        OperationalState::Testing => "Тестовая печать",
        OperationalState::Waiting => "Ожидание",
        OperationalState::CoverOpen => "Открыта крышка",
        OperationalState::PaperOut => "Нет бумаги",
        OperationalState::PaperJam => "Замятие бумаги",
        OperationalState::OperatorCall => "Требуется действие",
        OperationalState::ServiceError => "Сервисная ошибка",
        OperationalState::Offline => "Не в сети",
    }
}

/// Strips redundant nested transport/protocol prefixes before showing an error in UI widgets.
fn format_concise_error(raw: &str) -> String {
    let mut s = raw.trim();
    loop {
        let stripped = s
            .strip_prefix("Protocol error:")
            .or_else(|| s.strip_prefix("USB transport error:"))
            .or_else(|| s.strip_prefix("I/O error:"))
            .or_else(|| s.strip_prefix("Ошибка:"))
            .map(str::trim_start);
        match stripped {
            Some(next) => s = next,
            None => break,
        }
    }
    s.to_string()
}

/// Formats a concise error for single-line `adw::Toast`, trimming on word boundaries if long.
fn format_toast_error(raw: &str) -> String {
    const MAX_TOAST_CHARS: usize = 92;
    let concise = format_concise_error(raw);
    let prefixed = format!("Ошибка: {concise}");
    if prefixed.chars().count() <= MAX_TOAST_CHARS {
        return prefixed;
    }
    let suffix = "… (см. статус)";
    let keep = MAX_TOAST_CHARS.saturating_sub(suffix.chars().count());
    let mut byte_end = prefixed
        .char_indices()
        .nth(keep)
        .map(|(idx, _)| idx)
        .unwrap_or(prefixed.len());
    if let Some(last_space) = prefixed[..byte_end].rfind(' ') {
        if last_space > 20 {
            byte_end = last_space;
        }
    }
    format!("{}{}", prefixed[..byte_end].trim_end(), suffix)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_error_strips_nested_transport_prefixes_and_avoids_midword_chop() {
        let raw = "Protocol error: USB transport error: Принтер остался в Idle и не начал очистку (Regular / ColorOnly, токен 'group2') (прошивка разрешает: regular=[all, group1, group2], deep=[all, group1]).";
        assert_eq!(
            format_concise_error(raw),
            "Принтер остался в Idle и не начал очистку (Regular / ColorOnly, токен 'group2') (прошивка разрешает: regular=[all, group1, group2], deep=[all, group1])."
        );
        let toast = format_toast_error(raw);
        assert!(toast.starts_with("Ошибка: Принтер остался в Idle"));
        assert!(toast.ends_with("… (см. статус)"));
        assert!(!toast.contains("Protocol error"));
        assert!(!toast.contains("USB transport error"));
        assert!(toast.chars().count() <= 92);
    }

    #[test]
    fn hardware_task_finished_resets_ink_flush_confirmed_and_busy_flags() {
        let mut vm = StudioViewModel::default();
        vm.handle_event(StudioEvent::SetInkFlushConfirmation(true));
        vm.is_busy = true;
        vm.busy_operation = Some("InkFlush".to_string());
        let mut model = AppModel {
            vm,
            client: Arc::new(TokioMutex::new(None)),
            poll_notify: Arc::new(Notify::new()),
            configured_socket: None,
            pending_toasts: RefCell::new(Vec::new()),
            last_maint_feedback: None,
            last_scan_error: None,
            capabilities_fetched: false,
        };

        model.apply_hardware_task_finished(Ok("Done".to_string()));
        assert!(!model.vm.ink_flush_confirmed);
        assert!(!model.vm.is_busy);
        assert!(model.vm.busy_operation.is_none());
        assert!(model.vm.last_error.is_none());
        assert_eq!(model.vm.status_banner, "Done");

        model.vm.handle_event(StudioEvent::SetInkFlushConfirmation(true));
        model.vm.is_busy = true;
        model.apply_hardware_task_finished(Err("USB transport error: timeout".to_string()));
        assert!(!model.vm.ink_flush_confirmed);
        assert!(!model.vm.is_busy);
        assert_eq!(model.vm.last_error.as_deref(), Some("timeout"));

        model.vm.last_scan_path = Some(PathBuf::from("stale_scan.png"));
        model.vm.last_scan_thumb = Some(PathBuf::from("stale_thumb.png"));
        model.vm.is_busy = true;
        model.apply_scan_completed(Err("USB transport error: scanner lid open".to_string()));
        assert!(!model.vm.is_busy);
        assert_eq!(model.last_scan_error.as_deref(), Some("scanner lid open"));
        assert!(model.vm.last_scan_path.is_none());
        assert!(model.vm.last_scan_thumb.is_none());
        assert!(!model.pending_toasts.borrow().is_empty());

        let scan_path = PathBuf::from("scan.png");
        model.vm.is_busy = true;
        model.apply_scan_completed(Ok((1, Some(scan_path.clone()), None)));
        assert!(!model.vm.is_busy);
        assert!(model.last_scan_error.is_none());
        assert_eq!(model.vm.last_scan_path, Some(scan_path));

        let escaped = gtk::glib::markup_escape_text("<ivec:Cleaning> & A/B");
        assert_eq!(escaped.as_str(), "&lt;ivec:Cleaning&gt; &amp; A/B");
    }
}
