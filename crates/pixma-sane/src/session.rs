//! Safe Rust scanner session state machine for SANE backend.
//!
//! Manages SANE options, optical geometry, device communication via
//! `G2010Controller`, post-processing pipeline execution, and chunked
//! raster streaming.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};

use image::RgbImage;
use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_imageproc::execute_scan_pipeline;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};

use crate::sane_types::*;

/// Total number of options exported by the SANE backend (indices 0..12).
pub const NUM_OPTIONS: SANE_Int = 13;

// Static string and word constraint lists
static RESOLUTION_LIST: [SANE_Word; 9] = [8, 75, 100, 150, 200, 300, 400, 600, 1200];

static MODE_COLOR: &[u8] = b"Color\0";
static MODE_GRAY: &[u8] = b"Gray\0";
static MODE_LINEART: &[u8] = b"Lineart\0";
static MODE_LIST: SyncWrapper<[*const SANE_Char; 4]> = SyncWrapper([
    MODE_COLOR.as_ptr() as *const SANE_Char,
    MODE_GRAY.as_ptr() as *const SANE_Char,
    MODE_LINEART.as_ptr() as *const SANE_Char,
    std::ptr::null(),
]);

// Flatbed platen max dimensions in millimeters: 8.5" = 215.9 mm, 11.7" = 297.0 mm
const MAX_X_MM: f64 = 215.9;
const MAX_Y_MM: f64 = 297.0;

static RANGE_X: SANE_Range = SANE_Range {
    min: 0,
    max: 14149222, // sane_fix(215.9) = 14149222
    quant: 0,
};

static RANGE_Y: SANE_Range = SANE_Range {
    min: 0,
    max: 19464192, // sane_fix(297.0) = 19464192
    quant: 0,
};

/// Option descriptor definitions for all 13 supported options.
pub static OPTION_DESCRIPTORS: [SANE_Option_Descriptor; NUM_OPTIONS as usize] = [
    // Option 0: Option Count
    SANE_Option_Descriptor {
        name: b"\0".as_ptr() as *const SANE_Char,
        title: b"Number of options\0".as_ptr() as *const SANE_Char,
        desc: b"Read-only count of available options\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_INT,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Int>() as SANE_Int,
        cap: 0,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 1: resolution
    SANE_Option_Descriptor {
        name: b"resolution\0".as_ptr() as *const SANE_Char,
        title: b"Scan resolution\0".as_ptr() as *const SANE_Char,
        desc: b"Optical / hardware scan resolution in DPI\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_INT,
        unit: SANE_UNIT_DPI,
        size: std::mem::size_of::<SANE_Int>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_WORD_LIST,
        constraint: SANE_Constraint {
            word_list: RESOLUTION_LIST.as_ptr(),
        },
    },
    // Option 2: mode
    SANE_Option_Descriptor {
        name: b"mode\0".as_ptr() as *const SANE_Char,
        title: b"Scan mode\0".as_ptr() as *const SANE_Char,
        desc: b"Scan color acquisition mode\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_STRING,
        unit: SANE_UNIT_NONE,
        size: 16,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_STRING_LIST,
        constraint: SANE_Constraint {
            string_list: MODE_LIST.0.as_ptr(),
        },
    },
    // Option 3: tl-x
    SANE_Option_Descriptor {
        name: b"tl-x\0".as_ptr() as *const SANE_Char,
        title: b"Top-left x\0".as_ptr() as *const SANE_Char,
        desc: b"Top-left x position of scan area in mm\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_FIXED,
        unit: SANE_UNIT_MM,
        size: std::mem::size_of::<SANE_Fixed>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_RANGE,
        constraint: SANE_Constraint { range: &RANGE_X },
    },
    // Option 4: tl-y
    SANE_Option_Descriptor {
        name: b"tl-y\0".as_ptr() as *const SANE_Char,
        title: b"Top-left y\0".as_ptr() as *const SANE_Char,
        desc: b"Top-left y position of scan area in mm\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_FIXED,
        unit: SANE_UNIT_MM,
        size: std::mem::size_of::<SANE_Fixed>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_RANGE,
        constraint: SANE_Constraint { range: &RANGE_Y },
    },
    // Option 5: br-x
    SANE_Option_Descriptor {
        name: b"br-x\0".as_ptr() as *const SANE_Char,
        title: b"Bottom-right x\0".as_ptr() as *const SANE_Char,
        desc: b"Bottom-right x position of scan area in mm\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_FIXED,
        unit: SANE_UNIT_MM,
        size: std::mem::size_of::<SANE_Fixed>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_RANGE,
        constraint: SANE_Constraint { range: &RANGE_X },
    },
    // Option 6: br-y
    SANE_Option_Descriptor {
        name: b"br-y\0".as_ptr() as *const SANE_Char,
        title: b"Bottom-right y\0".as_ptr() as *const SANE_Char,
        desc: b"Bottom-right y position of scan area in mm\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_FIXED,
        unit: SANE_UNIT_MM,
        size: std::mem::size_of::<SANE_Fixed>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_RANGE,
        constraint: SANE_Constraint { range: &RANGE_Y },
    },
    // Option 7: deskew
    SANE_Option_Descriptor {
        name: b"deskew\0".as_ptr() as *const SANE_Char,
        title: b"Deskew\0".as_ptr() as *const SANE_Char,
        desc: b"Automatically detects skew angle and rotates image straight\0".as_ptr()
            as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 8: descreen
    SANE_Option_Descriptor {
        name: b"descreen\0".as_ptr() as *const SANE_Char,
        title: b"Descreen\0".as_ptr() as *const SANE_Char,
        desc: b"Removes halftone printing moire patterns via 2D-FFT\0".as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 9: unsharp-mask
    SANE_Option_Descriptor {
        name: b"unsharp-mask\0".as_ptr() as *const SANE_Char,
        title: b"Unsharp mask\0".as_ptr() as *const SANE_Char,
        desc: b"Enhances image edges and details using Gaussian unsharp masking\0".as_ptr()
            as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 10: remove-dust
    SANE_Option_Descriptor {
        name: b"remove-dust\0".as_ptr() as *const SANE_Char,
        title: b"Remove dust\0".as_ptr() as *const SANE_Char,
        desc: b"Filters dust particles and scratches using morphological median filtering\0"
            .as_ptr() as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 11: gutter-shadow
    SANE_Option_Descriptor {
        name: b"gutter-shadow\0".as_ptr() as *const SANE_Char,
        title: b"Gutter shadow\0".as_ptr() as *const SANE_Char,
        desc: b"Compensates for darkness and curvature shadows along book spine\0".as_ptr()
            as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
    // Option 12: auto-crop
    SANE_Option_Descriptor {
        name: b"auto-crop\0".as_ptr() as *const SANE_Char,
        title: b"Auto crop\0".as_ptr() as *const SANE_Char,
        desc: b"Automatically segments and crops document boundaries\0".as_ptr()
            as *const SANE_Char,
        type_: SANE_TYPE_BOOL,
        unit: SANE_UNIT_NONE,
        size: std::mem::size_of::<SANE_Word>() as SANE_Int,
        cap: SANE_CAP_SOFT_SELECT | SANE_CAP_SOFT_DETECT,
        constraint_type: SANE_CONSTRAINT_NONE,
        constraint: SANE_Constraint {
            word_list: std::ptr::null(),
        },
    },
];

/// Helper to execute an async future synchronously, safely handling nested runtimes.
fn run_async<F: std::future::Future>(f: F) -> F::Output {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(f))
    } else {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to build tokio runtime for SANE session");
        rt.block_on(f)
    }
}

/// SANE session managing scanner hardware options and acquisition state.
pub struct SaneScannerSession {
    pub resolution: ScanResolution,
    pub mode_str: String,
    pub tl_x: f64,
    pub tl_y: f64,
    pub br_x: f64,
    pub br_y: f64,
    pub deskew: bool,
    pub descreen: bool,
    pub unsharp_mask: bool,
    pub remove_dust: bool,
    pub gutter_shadow: bool,
    pub auto_crop: bool,

    // Hardware controller instance
    controller: Option<G2010Controller<AnyTransport>>,

    // Acquisition & streaming buffer
    buffer: Vec<u8>,
    read_offset: usize,
    scanning: bool,
    cancelled: AtomicBool,
    cached_parameters: Option<SANE_Parameters>,
}

impl SaneScannerSession {
    /// Creates a new scanner session with default settings.
    ///
    /// Automatically connects to physical hardware or falls back to `MockG2010Transport`.
    pub fn new() -> Self {
        Self {
            resolution: ScanResolution::Dpi300,
            mode_str: "Color".to_string(),
            tl_x: 0.0,
            tl_y: 0.0,
            br_x: MAX_X_MM,
            br_y: MAX_Y_MM,
            deskew: false,
            descreen: false,
            unsharp_mask: true,
            remove_dust: false,
            gutter_shadow: false,
            auto_crop: false,
            controller: None,
            buffer: Vec::new(),
            read_offset: 0,
            scanning: false,
            cancelled: AtomicBool::new(false),
            cached_parameters: None,
        }
    }

    /// Creates a session pre-configured with a custom controller (useful for unit tests).
    pub fn new_with_controller(controller: G2010Controller<AnyTransport>) -> Self {
        let mut session = Self::new();
        session.controller = Some(controller);
        session
    }

    /// Creates a session with in-memory mock hardware for tests.
    pub fn new_mock() -> Self {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
        Self::new_with_controller(controller)
    }

    /// Ensures an active controller is initialized.
    fn ensure_controller(&mut self) {
        if self.controller.is_some() {
            return;
        }

        let is_mock_forced = std::env::var("PIXMA_MOCK").as_deref() == Ok("1");
        let transport = if !is_mock_forced {
            if let Ok(devs) = discover_canon_devices() {
                if !devs.is_empty() {
                    match NusbTransport::open_first() {
                        Ok(usb) => AnyTransport::Usb(usb),
                        Err(_) => AnyTransport::Mock(Box::new(MockG2010Transport::new())),
                    }
                } else {
                    AnyTransport::Mock(Box::new(MockG2010Transport::new()))
                }
            } else {
                AnyTransport::Mock(Box::new(MockG2010Transport::new()))
            }
        } else {
            AnyTransport::Mock(Box::new(MockG2010Transport::new()))
        };

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        self.controller = Some(G2010Controller::new(transport, supervisor));
    }

    /// Returns the scan color mode derived from the option string.
    pub fn color_mode(&self) -> ScanColorMode {
        match self.mode_str.as_str() {
            "Gray" => ScanColorMode::Grayscale8Bit,
            "Lineart" => ScanColorMode::Lineart1Bit,
            _ => ScanColorMode::Color24Bit,
        }
    }

    /// Calculates scan pixel geometry clamped to platen bounds.
    pub fn calculate_geometry(&self) -> ScanGeometry {
        let dpi = self.resolution.as_dpi() as f64;
        let (max_w, max_h) = self.resolution.max_flatbed_pixels();

        let x_px = (self.tl_x / 25.4 * dpi).round() as u32;
        let y_px = (self.tl_y / 25.4 * dpi).round() as u32;
        let br_x_px = (self.br_x / 25.4 * dpi).round() as u32;
        let br_y_px = (self.br_y / 25.4 * dpi).round() as u32;

        let x_offset = x_px.min(max_w.saturating_sub(1));
        let y_offset = y_px.min(max_h.saturating_sub(1));

        let width = br_x_px
            .saturating_sub(x_offset)
            .clamp(1, max_w.saturating_sub(x_offset));
        let height = br_y_px
            .saturating_sub(y_offset)
            .clamp(1, max_h.saturating_sub(y_offset));

        ScanGeometry {
            x_offset,
            y_offset,
            width,
            height,
        }
    }

    /// Computes parameters based on current option settings or active scan output.
    pub fn get_parameters(&self) -> SANE_Parameters {
        if let Some(cached) = self.cached_parameters {
            return cached;
        }

        let geom = self.calculate_geometry();
        let width = geom.width as SANE_Int;
        let height = geom.height as SANE_Int;

        match self.color_mode() {
            ScanColorMode::Color24Bit => SANE_Parameters {
                format: SANE_FRAME_RGB,
                last_frame: SANE_TRUE,
                bytes_per_line: width * 3,
                pixels_per_line: width,
                lines: height,
                depth: 8,
            },
            ScanColorMode::Grayscale8Bit => SANE_Parameters {
                format: SANE_FRAME_GRAY,
                last_frame: SANE_TRUE,
                bytes_per_line: width,
                pixels_per_line: width,
                lines: height,
                depth: 8,
            },
            ScanColorMode::Lineart1Bit => SANE_Parameters {
                format: SANE_FRAME_GRAY,
                last_frame: SANE_TRUE,
                bytes_per_line: (width + 7) / 8,
                pixels_per_line: width,
                lines: height,
                depth: 1,
            },
        }
    }

    /// Starts acquisition: acquires raw scanlines, applies pipeline, buffers raster bytes.
    pub fn start_scan(&mut self) -> SANE_Status {
        self.cancelled.store(false, Ordering::SeqCst);
        self.ensure_controller();

        let geom = self.calculate_geometry();
        let color_mode = self.color_mode();
        let params = ScanParameters {
            resolution: self.resolution,
            color_mode,
            geometry: geom,
            auto_multi_crop: self.auto_crop,
            deskew: self.deskew,
            descreen_moire: self.descreen,
            unsharp_mask: self.unsharp_mask,
            remove_dust_scratches: self.remove_dust,
            correct_gutter_shadow: self.gutter_shadow,
            gamma: 2.2,
        };

        let controller = match self.controller.as_mut() {
            Some(c) => c,
            None => return SANE_STATUS_IO_ERROR,
        };

        // Acquire raw bytes from hardware controller
        let raw_bytes = match run_async(controller.acquire_raw_scan(&params)) {
            Ok(b) => b,
            Err(e) => {
                tracing::error!("Scanner raw acquisition error: {e}");
                return SANE_STATUS_IO_ERROR;
            }
        };

        if self.cancelled.load(Ordering::SeqCst) {
            return SANE_STATUS_CANCELLED;
        }

        // Convert raw byte stream into RgbImage
        let width = geom.width;
        let height = geom.height;
        let bpp = color_mode.bytes_per_pixel();
        let expected_len = (width as usize) * (height as usize) * bpp;

        if raw_bytes.len() < expected_len {
            tracing::error!(
                "Acquired scan payload length mismatch: got {}, expected {}",
                raw_bytes.len(),
                expected_len
            );
            return SANE_STATUS_IO_ERROR;
        }

        let rgb_img = if bpp == 3 {
            match RgbImage::from_raw(width, height, raw_bytes[..expected_len].to_vec()) {
                Some(img) => img,
                None => return SANE_STATUS_IO_ERROR,
            }
        } else {
            // Expand 8-bit Gray to RGB for post-processing pipeline
            let mut rgb_buf = Vec::with_capacity((width * height * 3) as usize);
            for &g in &raw_bytes[..expected_len] {
                rgb_buf.extend_from_slice(&[g, g, g]);
            }
            match RgbImage::from_raw(width, height, rgb_buf) {
                Some(img) => img,
                None => return SANE_STATUS_IO_ERROR,
            }
        };

        // Run full ScanGear post-processing restoration pipeline
        let processed_images = match execute_scan_pipeline(rgb_img, &params) {
            Ok(imgs) => imgs,
            Err(e) => {
                tracing::error!("Scanner post-processing error: {e:?}");
                return SANE_STATUS_IO_ERROR;
            }
        };

        if processed_images.is_empty() {
            return SANE_STATUS_IO_ERROR;
        }

        let out_img = &processed_images[0];
        let final_w = out_img.width() as SANE_Int;
        let final_h = out_img.height() as SANE_Int;

        // Pack output raster bytes according to SANE frame format and bit depth
        match color_mode {
            ScanColorMode::Color24Bit => {
                self.buffer = out_img.as_raw().clone();
                self.cached_parameters = Some(SANE_Parameters {
                    format: SANE_FRAME_RGB,
                    last_frame: SANE_TRUE,
                    bytes_per_line: final_w * 3,
                    pixels_per_line: final_w,
                    lines: final_h,
                    depth: 8,
                });
            }
            ScanColorMode::Grayscale8Bit => {
                let mut gray_buf = Vec::with_capacity((final_w * final_h) as usize);
                for p in out_img.pixels() {
                    gray_buf.push(p[0]);
                }
                self.buffer = gray_buf;
                self.cached_parameters = Some(SANE_Parameters {
                    format: SANE_FRAME_GRAY,
                    last_frame: SANE_TRUE,
                    bytes_per_line: final_w,
                    pixels_per_line: final_w,
                    lines: final_h,
                    depth: 8,
                });
            }
            ScanColorMode::Lineart1Bit => {
                let bpl = ((final_w + 7) / 8) as usize;
                let mut lineart_buf = vec![0u8; bpl * (final_h as usize)];
                for y in 0..out_img.height() {
                    let row = (y as usize) * bpl;
                    for x in 0..out_img.width() {
                        let px = out_img.get_pixel(x, y);
                        // Standard SANE: 1 = light/white, 0 = dark/black
                        if px[0] >= 128 {
                            lineart_buf[row + (x as usize / 8)] |= 1 << (7 - (x % 8));
                        }
                    }
                }
                self.buffer = lineart_buf;
                self.cached_parameters = Some(SANE_Parameters {
                    format: SANE_FRAME_GRAY,
                    last_frame: SANE_TRUE,
                    bytes_per_line: bpl as SANE_Int,
                    pixels_per_line: final_w,
                    lines: final_h,
                    depth: 1,
                });
            }
        }

        self.read_offset = 0;
        self.scanning = true;
        SANE_STATUS_GOOD
    }

    /// Reads up to `out.len()` bytes from the scanned raster stream.
    pub fn read_chunk(&mut self, out: &mut [u8]) -> Result<usize, SANE_Status> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(SANE_STATUS_CANCELLED);
        }
        if !self.scanning {
            return Err(SANE_STATUS_INVAL);
        }

        if self.read_offset >= self.buffer.len() {
            self.scanning = false;
            return Err(SANE_STATUS_EOF);
        }

        let available = self.buffer.len() - self.read_offset;
        let count = out.len().min(available);
        out[..count].copy_from_slice(&self.buffer[self.read_offset..self.read_offset + count]);
        self.read_offset += count;

        Ok(count)
    }

    /// Cancels an in-progress or queued scan operation.
    pub fn cancel(&mut self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.scanning = false;
        self.read_offset = 0;
        self.buffer.clear();
        self.cached_parameters = None;
    }

    /// Handles `sane_control_option` queries and updates.
    pub unsafe fn control_option(
        &mut self,
        option: SANE_Int,
        action: SANE_Action,
        val: *mut c_void,
        info: *mut SANE_Int,
    ) -> SANE_Status {
        if !(0..NUM_OPTIONS).contains(&option) {
            return SANE_STATUS_INVAL;
        }

        match action {
            SANE_ACTION_GET_VALUE => {
                if val.is_null() {
                    return SANE_STATUS_INVAL;
                }
                match option {
                    0 => *(val as *mut SANE_Int) = NUM_OPTIONS,
                    1 => *(val as *mut SANE_Int) = self.resolution.as_dpi() as SANE_Int,
                    2 => {
                        let dst = val as *mut u8;
                        let bytes = self.mode_str.as_bytes();
                        let n = bytes.len().min(15);
                        std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst, n);
                        *dst.add(n) = 0;
                    }
                    3 => *(val as *mut SANE_Fixed) = sane_fix(self.tl_x),
                    4 => *(val as *mut SANE_Fixed) = sane_fix(self.tl_y),
                    5 => *(val as *mut SANE_Fixed) = sane_fix(self.br_x),
                    6 => *(val as *mut SANE_Fixed) = sane_fix(self.br_y),
                    7 => {
                        *(val as *mut SANE_Word) = if self.deskew { SANE_TRUE } else { SANE_FALSE }
                    }
                    8 => {
                        *(val as *mut SANE_Word) =
                            if self.descreen { SANE_TRUE } else { SANE_FALSE }
                    }
                    9 => {
                        *(val as *mut SANE_Word) = if self.unsharp_mask {
                            SANE_TRUE
                        } else {
                            SANE_FALSE
                        }
                    }
                    10 => {
                        *(val as *mut SANE_Word) = if self.remove_dust {
                            SANE_TRUE
                        } else {
                            SANE_FALSE
                        }
                    }
                    11 => {
                        *(val as *mut SANE_Word) = if self.gutter_shadow {
                            SANE_TRUE
                        } else {
                            SANE_FALSE
                        }
                    }
                    12 => {
                        *(val as *mut SANE_Word) = if self.auto_crop {
                            SANE_TRUE
                        } else {
                            SANE_FALSE
                        }
                    }
                    _ => return SANE_STATUS_INVAL,
                }
                SANE_STATUS_GOOD
            }
            SANE_ACTION_SET_VALUE => {
                if val.is_null() {
                    return SANE_STATUS_INVAL;
                }
                let mut reload_params = false;

                match option {
                    0 => return SANE_STATUS_INVAL, // Option count is read-only
                    1 => {
                        let dpi = *(val as *const SANE_Int) as u32;
                        match ScanResolution::from_dpi(dpi) {
                            Ok(res) => {
                                self.resolution = res;
                                reload_params = true;
                            }
                            Err(_) => return SANE_STATUS_INVAL,
                        }
                    }
                    2 => {
                        let s = std::ffi::CStr::from_ptr(val as *const SANE_Char)
                            .to_string_lossy()
                            .into_owned();
                        match s.as_str() {
                            "Color" | "Gray" | "Lineart" => {
                                self.mode_str = s;
                                reload_params = true;
                            }
                            _ => return SANE_STATUS_INVAL,
                        }
                    }
                    3 => {
                        let mm = sane_unfix(*(val as *const SANE_Fixed)).clamp(0.0, MAX_X_MM);
                        self.tl_x = mm;
                        reload_params = true;
                    }
                    4 => {
                        let mm = sane_unfix(*(val as *const SANE_Fixed)).clamp(0.0, MAX_Y_MM);
                        self.tl_y = mm;
                        reload_params = true;
                    }
                    5 => {
                        let mm = sane_unfix(*(val as *const SANE_Fixed)).clamp(0.0, MAX_X_MM);
                        self.br_x = mm;
                        reload_params = true;
                    }
                    6 => {
                        let mm = sane_unfix(*(val as *const SANE_Fixed)).clamp(0.0, MAX_Y_MM);
                        self.br_y = mm;
                        reload_params = true;
                    }
                    7 => self.deskew = *(val as *const SANE_Word) != 0,
                    8 => self.descreen = *(val as *const SANE_Word) != 0,
                    9 => self.unsharp_mask = *(val as *const SANE_Word) != 0,
                    10 => self.remove_dust = *(val as *const SANE_Word) != 0,
                    11 => self.gutter_shadow = *(val as *const SANE_Word) != 0,
                    12 => self.auto_crop = *(val as *const SANE_Word) != 0,
                    _ => return SANE_STATUS_INVAL,
                }

                if reload_params {
                    self.cached_parameters = None;
                    if !info.is_null() {
                        *info |= SANE_INFO_RELOAD_PARAMS;
                    }
                }
                SANE_STATUS_GOOD
            }
            SANE_ACTION_SET_AUTO => SANE_STATUS_UNSUPPORTED,
        }
    }
}

impl Default for SaneScannerSession {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_options_lifecycle() {
        let mut session = SaneScannerSession::new_mock();

        unsafe {
            // Read option count
            let mut count: SANE_Int = 0;
            let status = session.control_option(
                0,
                SANE_ACTION_GET_VALUE,
                &mut count as *mut _ as *mut c_void,
                std::ptr::null_mut(),
            );
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_eq!(count, NUM_OPTIONS);

            // Change resolution to 600 DPI
            let mut res: SANE_Int = 600;
            let mut info: SANE_Int = 0;
            let status = session.control_option(
                1,
                SANE_ACTION_SET_VALUE,
                &mut res as *mut _ as *mut c_void,
                &mut info,
            );
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_eq!(session.resolution, ScanResolution::Dpi600);
            assert_ne!(info & SANE_INFO_RELOAD_PARAMS, 0);

            // Read mode
            let mut mode_buf = [0u8; 16];
            let status = session.control_option(
                2,
                SANE_ACTION_GET_VALUE,
                mode_buf.as_mut_ptr() as *mut c_void,
                std::ptr::null_mut(),
            );
            assert_eq!(status, SANE_STATUS_GOOD);
            let mode_str = std::ffi::CStr::from_ptr(mode_buf.as_ptr() as *const SANE_Char)
                .to_str()
                .unwrap();
            assert_eq!(mode_str, "Color");

            // Change mode to Gray
            let new_mode = b"Gray\0";
            let status = session.control_option(
                2,
                SANE_ACTION_SET_VALUE,
                new_mode.as_ptr() as *mut c_void,
                std::ptr::null_mut(),
            );
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_eq!(session.mode_str, "Gray");

            // Test boolean options
            let mut val: SANE_Word = SANE_TRUE;
            assert_eq!(
                session.control_option(
                    7,
                    SANE_ACTION_SET_VALUE,
                    &mut val as *mut _ as *mut c_void,
                    std::ptr::null_mut()
                ),
                SANE_STATUS_GOOD
            );
            assert!(session.deskew);
        }
    }

    #[test]
    fn test_mock_scan_execution() {
        let mut session = SaneScannerSession::new_mock();
        session.resolution = ScanResolution::Dpi75;
        session.br_x = 50.0; // small region for speed
        session.br_y = 50.0;

        let status = session.start_scan();
        assert_eq!(status, SANE_STATUS_GOOD);

        let params = session.get_parameters();
        assert_eq!(params.format, SANE_FRAME_RGB);
        assert_eq!(params.depth, 8);
        assert!(params.lines > 0);
        assert!(params.pixels_per_line > 0);

        let mut read_buf = [0u8; 512];
        let mut total_read = 0;
        loop {
            match session.read_chunk(&mut read_buf) {
                Ok(n) => total_read += n,
                Err(SANE_STATUS_EOF) => break,
                Err(e) => panic!("Unexpected read error: {e:?}"),
            }
        }
        assert!(total_read > 0);
        assert_eq!(total_read, (params.lines * params.bytes_per_line) as usize);
    }
}
