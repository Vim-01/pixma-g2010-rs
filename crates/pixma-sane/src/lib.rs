//! Pure-Rust SANE (Scanner Access Now Easy) Standard v1.04 backend for Canon PIXMA G2010.
//!
//! Provides the standard C ABI interface (`libsane-pixma_g2010.so`), option configuration,
//! hardware communication via `G2010Controller`, ScanGear post-processing, and streaming raster readout.

#![allow(clippy::manual_c_str_literals)]
#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]

pub mod ffi;
pub mod sane_types;
pub mod session;

pub use ffi::*;
pub use sane_types::*;
pub use session::{SaneScannerSession, NUM_OPTIONS, OPTION_DESCRIPTORS};

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::c_void;

    #[test]
    fn test_sane_c_abi_complete_lifecycle() {
        unsafe {
            // 1. sane_init
            let mut version_code: SANE_Int = 0;
            let status = sane_init(&mut version_code, None);
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_eq!(version_code, sane_version_code(1, 0, 4));

            // 2. sane_get_devices (live-probed: empty when offline unless hardware/pixmad is connected)
            let mut dev_list: *const *const SANE_Device = std::ptr::null();
            let status = sane_get_devices(&mut dev_list, SANE_FALSE);
            assert_eq!(status, SANE_STATUS_GOOD);
            assert!(!dev_list.is_null());
            let first_dev = *dev_list;
            if !first_dev.is_null() {
                let dev_name = std::ffi::CStr::from_ptr((*first_dev).name)
                    .to_str()
                    .unwrap();
                assert!(dev_name == "pixma:G2010" || dev_name == "pixma:G2010:mock");
            }

            // 3. sane_open (explicit mock device suffix without mutating process env vars)
            let mut handle: SANE_Handle = std::ptr::null_mut();
            let status = sane_open(b"pixma:G2010:mock\0".as_ptr() as *const SANE_Char, &mut handle);
            assert_eq!(status, SANE_STATUS_GOOD);
            assert!(!handle.is_null());

            // 4. sane_get_option_descriptor for all options
            for opt_idx in 0..NUM_OPTIONS {
                let desc = sane_get_option_descriptor(handle, opt_idx);
                assert!(!desc.is_null(), "Option {opt_idx} descriptor is null");
            }
            // Out-of-bounds option returns null
            assert!(sane_get_option_descriptor(handle, 999).is_null());

            // 5. sane_control_option (Set resolution = 150 DPI)
            let mut res: SANE_Int = 150;
            let mut info: SANE_Int = 0;
            let status = sane_control_option(
                handle,
                1,
                SANE_ACTION_SET_VALUE,
                &mut res as *mut _ as *mut c_void,
                &mut info,
            );
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_ne!(info & SANE_INFO_RELOAD_PARAMS, 0);

            // Set small geometry for fast test execution: 25mm x 25mm
            let mut br_x: SANE_Fixed = sane_fix(25.0);
            let mut br_y: SANE_Fixed = sane_fix(25.0);
            assert_eq!(
                sane_control_option(
                    handle,
                    5,
                    SANE_ACTION_SET_VALUE,
                    &mut br_x as *mut _ as *mut c_void,
                    std::ptr::null_mut(),
                ),
                SANE_STATUS_GOOD
            );
            assert_eq!(
                sane_control_option(
                    handle,
                    6,
                    SANE_ACTION_SET_VALUE,
                    &mut br_y as *mut _ as *mut c_void,
                    std::ptr::null_mut(),
                ),
                SANE_STATUS_GOOD
            );

            // 6. sane_get_parameters
            let mut params = SANE_Parameters {
                format: SANE_FRAME_GRAY,
                last_frame: SANE_FALSE,
                bytes_per_line: 0,
                pixels_per_line: 0,
                lines: 0,
                depth: 0,
            };
            let status = sane_get_parameters(handle, &mut params);
            assert_eq!(status, SANE_STATUS_GOOD);
            assert_eq!(params.format, SANE_FRAME_RGB);
            assert_eq!(params.depth, 8);
            assert!(params.pixels_per_line > 0);
            assert!(params.lines > 0);

            // 7. sane_start
            let status = sane_start(handle);
            assert_eq!(status, SANE_STATUS_GOOD);

            // Parameters after start
            let status = sane_get_parameters(handle, &mut params);
            assert_eq!(status, SANE_STATUS_GOOD);
            let total_expected_bytes = (params.lines * params.bytes_per_line) as usize;

            // 8. sane_read until SANE_STATUS_EOF
            let mut read_buf = [0u8; 1024];
            let mut total_bytes_read = 0;
            loop {
                let mut chunk_len: SANE_Int = 0;
                let status = sane_read(
                    handle,
                    read_buf.as_mut_ptr(),
                    read_buf.len() as SANE_Int,
                    &mut chunk_len,
                );
                if status == SANE_STATUS_EOF {
                    assert_eq!(chunk_len, 0);
                    break;
                }
                assert_eq!(status, SANE_STATUS_GOOD);
                assert!(chunk_len > 0);
                total_bytes_read += chunk_len as usize;
            }
            assert_eq!(total_bytes_read, total_expected_bytes);

            // 9. sane_cancel
            sane_cancel(handle);

            // 10. sane_close
            sane_close(handle);

            // 11. sane_exit
            sane_exit();
        }
    }

    #[test]
    fn test_sane_strstatus_messages() {
        unsafe {
            let str_good = sane_strstatus(SANE_STATUS_GOOD);
            assert_eq!(
                std::ffi::CStr::from_ptr(str_good).to_str().unwrap(),
                "Success"
            );

            let str_eof = sane_strstatus(SANE_STATUS_EOF);
            assert_eq!(
                std::ffi::CStr::from_ptr(str_eof).to_str().unwrap(),
                "End of file reached"
            );

            let str_io = sane_strstatus(SANE_STATUS_IO_ERROR);
            assert_eq!(
                std::ffi::CStr::from_ptr(str_io).to_str().unwrap(),
                "Error during device I/O"
            );
        }
    }

    #[test]
    fn test_null_pointers_do_not_segfault() {
        unsafe {
            assert_eq!(
                sane_open(std::ptr::null(), std::ptr::null_mut()),
                SANE_STATUS_INVAL
            );
            assert_eq!(sane_start(std::ptr::null_mut()), SANE_STATUS_INVAL);
            assert_eq!(
                sane_get_parameters(std::ptr::null_mut(), std::ptr::null_mut()),
                SANE_STATUS_INVAL
            );
            assert_eq!(
                sane_read(
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    0,
                    std::ptr::null_mut()
                ),
                SANE_STATUS_INVAL
            );
            sane_cancel(std::ptr::null_mut());
            sane_close(std::ptr::null_mut());
        }
    }

    #[test]
    fn test_sane_open_validates_device_names() {
        unsafe {
            let mut version: SANE_Int = 0;
            assert_eq!(sane_init(&mut version, None), SANE_STATUS_GOOD);

            let mut handle: SANE_Handle = std::ptr::null_mut();
            assert_eq!(
                sane_open(std::ptr::null(), &mut handle),
                SANE_STATUS_INVAL
            );
            assert!(handle.is_null());

            assert_eq!(
                sane_open(b"pixma:UNKNOWN\0".as_ptr() as *const SANE_Char, &mut handle),
                SANE_STATUS_INVAL
            );
            assert!(handle.is_null());

            // Live device name opens cleanly (lazy connect) and closes cleanly
            assert_eq!(
                sane_open(b"pixma:G2010\0".as_ptr() as *const SANE_Char, &mut handle),
                SANE_STATUS_GOOD
            );
            assert!(!handle.is_null());
            sane_close(handle);

            // Empty string also defaults to live session
            let mut empty_handle: SANE_Handle = std::ptr::null_mut();
            assert_eq!(
                sane_open(b"\0".as_ptr() as *const SANE_Char, &mut empty_handle),
                SANE_STATUS_GOOD
            );
            sane_close(empty_handle);

            sane_exit();
        }
    }

    #[test]
    fn test_select_device_list_probe_modes() {
        unsafe {
            let offline = crate::ffi::select_device_list(false, false);
            assert!((*offline).is_null(), "Offline probe must return empty device list");

            let live = crate::ffi::select_device_list(false, true);
            assert!(!(*live).is_null());
            let live_name = std::ffi::CStr::from_ptr((**live).name).to_str().unwrap();
            assert_eq!(live_name, "pixma:G2010");

            let mock_only = crate::ffi::select_device_list(true, false);
            assert!(!(*mock_only).is_null());
            let mock_name = std::ffi::CStr::from_ptr((**mock_only).name)
                .to_str()
                .unwrap();
            assert_eq!(mock_name, "pixma:G2010:mock");
        }
    }
}
