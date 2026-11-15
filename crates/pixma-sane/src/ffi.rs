//! Standard SANE Standard v1.04 C ABI exported entrypoints.
//!
//! Provides the external C ABI entrypoints conforming to SANE Standard Version 1.04.
//! Every `extern "C"` function wraps all logic in `std::panic::catch_unwind` and
//! returns `SANE_STATUS_IO_ERROR` (or a safe fallback) in the event of an internal
//! panic, completely preventing undefined behavior across FFI boundaries.

use std::ffi::c_void;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::sane_types::*;
use crate::session::{SaneScannerSession, NUM_OPTIONS, OPTION_DESCRIPTORS};

static IS_INITIALIZED: AtomicBool = AtomicBool::new(false);

static CANON_G2010_DEVICE: SANE_Device = SANE_Device {
    name: b"pixma:G2010\0".as_ptr() as *const SANE_Char,
    vendor: b"Canon\0".as_ptr() as *const SANE_Char,
    model: b"PIXMA G2010 series\0".as_ptr() as *const SANE_Char,
    type_: b"flatbed scanner\0".as_ptr() as *const SANE_Char,
};

static CANON_G2010_MOCK_DEVICE: SANE_Device = SANE_Device {
    name: b"pixma:G2010:mock\0".as_ptr() as *const SANE_Char,
    vendor: b"Canon\0".as_ptr() as *const SANE_Char,
    model: b"PIXMA G2010 series (Mock)\0".as_ptr() as *const SANE_Char,
    type_: b"flatbed scanner\0".as_ptr() as *const SANE_Char,
};

static LIVE_DEVICE_LIST: SyncWrapper<[*const SANE_Device; 2]> =
    SyncWrapper([&CANON_G2010_DEVICE, std::ptr::null()]);

static MOCK_DEVICE_LIST: SyncWrapper<[*const SANE_Device; 2]> =
    SyncWrapper([&CANON_G2010_MOCK_DEVICE, std::ptr::null()]);

static EMPTY_DEVICE_LIST: SyncWrapper<[*const SANE_Device; 1]> =
    SyncWrapper([std::ptr::null()]);

fn is_live_scanner_available() -> bool {
    if let Some(explicit) = std::env::var_os("PIXMA_SOCKET").filter(|v| !v.is_empty()) {
        return std::os::unix::net::UnixStream::connect(explicit).is_ok();
    }
    for sock in pixma_daemon::default_socket_candidates() {
        if sock.exists() && std::os::unix::net::UnixStream::connect(&sock).is_ok() {
            return true;
        }
    }
    !pixma_transport::nusb_transport::discover_canon_devices()
        .unwrap_or_default()
        .is_empty()
}

pub(crate) fn select_device_list(
    is_mock_env: bool,
    hw_available: bool,
) -> *const *const SANE_Device {
    if is_mock_env {
        MOCK_DEVICE_LIST.0.as_ptr()
    } else if hw_available {
        LIVE_DEVICE_LIST.0.as_ptr()
    } else {
        EMPTY_DEVICE_LIST.0.as_ptr()
    }
}

/// SANE v1.04 initialization entrypoint.
#[no_mangle]
pub unsafe extern "C" fn sane_init(
    version_code: *mut SANE_Int,
    _authorize: SANE_Auth_Callback,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if !version_code.is_null() {
            *version_code = sane_version_code(1, 0, 4);
        }
        IS_INITIALIZED.store(true, Ordering::SeqCst);
        SANE_STATUS_GOOD
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// SANE v1.04 teardown entrypoint.
#[no_mangle]
pub unsafe extern "C" fn sane_exit() {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        IS_INITIALIZED.store(false, Ordering::SeqCst);
    }));
}

/// Enumerates connected Canon PIXMA scanner devices (returns empty list when offline).
#[no_mangle]
pub unsafe extern "C" fn sane_get_devices(
    device_list: *mut *const *const SANE_Device,
    _local_only: SANE_Bool,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if device_list.is_null() {
            return SANE_STATUS_INVAL;
        }
        let is_mock_env = std::env::var("PIXMA_MOCK").as_deref() == Ok("1");
        let hw_available = !is_mock_env && is_live_scanner_available();
        *device_list = select_device_list(is_mock_env, hw_available);
        SANE_STATUS_GOOD
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Opens a scanner device session.
#[no_mangle]
pub unsafe extern "C" fn sane_open(
    name: SANE_String_Const,
    handle: *mut SANE_Handle,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if name.is_null() || handle.is_null() {
            return SANE_STATUS_INVAL;
        }
        let Ok(name_str) = std::ffi::CStr::from_ptr(name).to_str() else {
            return SANE_STATUS_INVAL;
        };
        let session = match name_str {
            "" | "pixma:G2010" => SaneScannerSession::new(),
            "pixma:G2010:mock" => SaneScannerSession::new_mock(),
            _ => return SANE_STATUS_INVAL,
        };
        *handle = Box::into_raw(Box::new(session)) as SANE_Handle;
        SANE_STATUS_GOOD
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Closes an open scanner device session.
#[no_mangle]
pub unsafe extern "C" fn sane_close(handle: SANE_Handle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !handle.is_null() {
            drop(Box::from_raw(handle as *mut SaneScannerSession));
        }
    }));
}

/// Queries metadata for a specific option index.
#[no_mangle]
pub unsafe extern "C" fn sane_get_option_descriptor(
    handle: SANE_Handle,
    option: SANE_Int,
) -> *const SANE_Option_Descriptor {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || !(0..NUM_OPTIONS).contains(&option) {
            return std::ptr::null();
        }
        &OPTION_DESCRIPTORS[option as usize]
    }))
    .unwrap_or(std::ptr::null())
}

/// Gets, sets, or auto-configures an option value.
#[no_mangle]
pub unsafe extern "C" fn sane_control_option(
    handle: SANE_Handle,
    option: SANE_Int,
    action: SANE_Action,
    value: *mut c_void,
    info: *mut SANE_Int,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return SANE_STATUS_INVAL;
        }
        let session = &mut *(handle as *mut SaneScannerSession);
        session.control_option(option, action, value, info)
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Queries current image format and dimensions.
#[no_mangle]
pub unsafe extern "C" fn sane_get_parameters(
    handle: SANE_Handle,
    params: *mut SANE_Parameters,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || params.is_null() {
            return SANE_STATUS_INVAL;
        }
        let session = &*(handle as *const SaneScannerSession);
        *params = session.get_parameters();
        SANE_STATUS_GOOD
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Begins hardware optical acquisition.
#[no_mangle]
pub unsafe extern "C" fn sane_start(handle: SANE_Handle) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return SANE_STATUS_INVAL;
        }
        let session = &mut *(handle as *mut SaneScannerSession);
        session.start_scan()
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Reads image scanline data chunk from backend buffer.
#[no_mangle]
pub unsafe extern "C" fn sane_read(
    handle: SANE_Handle,
    data: *mut SANE_Byte,
    max_length: SANE_Int,
    length: *mut SANE_Int,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || data.is_null() || length.is_null() || max_length <= 0 {
            return SANE_STATUS_INVAL;
        }
        let session = &mut *(handle as *mut SaneScannerSession);
        let out_slice = std::slice::from_raw_parts_mut(data, max_length as usize);
        match session.read_chunk(out_slice) {
            Ok(n) => {
                *length = n as SANE_Int;
                SANE_STATUS_GOOD
            }
            Err(status) => {
                *length = 0;
                status
            }
        }
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Cancels active scan and frees buffers.
#[no_mangle]
pub unsafe extern "C" fn sane_cancel(handle: SANE_Handle) {
    let _ = catch_unwind(AssertUnwindSafe(|| {
        if !handle.is_null() {
            let session = &mut *(handle as *mut SaneScannerSession);
            session.cancel();
        }
    }));
}

/// Sets I/O mode (blocking or non-blocking).
#[no_mangle]
pub unsafe extern "C" fn sane_set_io_mode(
    handle: SANE_Handle,
    _non_blocking: SANE_Bool,
) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() {
            return SANE_STATUS_INVAL;
        }
        SANE_STATUS_GOOD
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Queries file descriptor for `select` / `poll` (unsupported for synchronous USB).
#[no_mangle]
pub unsafe extern "C" fn sane_get_select_fd(handle: SANE_Handle, fd: *mut SANE_Int) -> SANE_Status {
    catch_unwind(AssertUnwindSafe(|| {
        if handle.is_null() || fd.is_null() {
            return SANE_STATUS_INVAL;
        }
        SANE_STATUS_UNSUPPORTED
    }))
    .unwrap_or(SANE_STATUS_IO_ERROR)
}

/// Returns a descriptive English error string for a `SANE_Status` code.
#[no_mangle]
pub unsafe extern "C" fn sane_strstatus(status: SANE_Status) -> SANE_String_Const {
    catch_unwind(AssertUnwindSafe(|| match status {
        SANE_STATUS_GOOD => b"Success\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_UNSUPPORTED => b"Operation not supported\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_CANCELLED => b"Operation was cancelled\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_DEVICE_BUSY => b"Device busy\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_INVAL => b"Data or argument is invalid\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_EOF => b"End of file reached\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_JAMMED => b"Document feeder jammed\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_NO_DOCS => b"Document feeder out of documents\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_COVER_OPEN => b"Scanner cover open\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_IO_ERROR => b"Error during device I/O\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_NO_MEM => b"Out of memory\0".as_ptr() as *const SANE_Char,
        SANE_STATUS_ACCESS_DENIED => b"Access to resource denied\0".as_ptr() as *const SANE_Char,
    }))
    .unwrap_or(b"Error during device I/O\0".as_ptr() as *const SANE_Char)
}
