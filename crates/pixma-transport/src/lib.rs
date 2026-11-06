//! Asynchronous USB transport, cooperative session locking, virtual G2010 hardware emulator,
//! and high-level safety controller for Canon PIXMA G-series multifunction devices.

pub mod controller;
pub mod mock_device;
pub mod nusb_transport;
pub mod traits;

pub use controller::{AnyTransport, G2010Controller};
pub use mock_device::{MockG2010Transport, G2010_IEEE1284_DEVICE_ID};
pub use nusb_transport::{discover_canon_devices, FileLockGuard, NusbTransport, SUPPORTED_PIDS};
pub use traits::UsbTransport;

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::errors::{DriverError, SafetyInterlockError};
    use pixma_core::maintenance::{
        CleaningTarget, CleaningType, MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor,
        RollerCleanTarget, TestPrintType, INK_FLUSH_CONFIRMATION_TOKEN,
    };
    use pixma_core::models::PrinterModel;
    use pixma_core::print_params::PrintJobConfig;
    use pixma_core::safety::ValidatedRasterLine;
    use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};

    #[tokio::test]
    async fn test_mock_transport_get_device_id() {
        let mut mock = MockG2010Transport::new();
        assert_eq!(mock.model(), PrinterModel::G2010);

        let device_id = mock.get_device_id().await.expect("Device ID query failed");
        assert_eq!(device_id, G2010_IEEE1284_DEVICE_ID);
        assert!(device_id.contains("MFG:Canon"));
        assert!(device_id.contains("MDL:G2010 series"));
    }

    #[tokio::test]
    async fn test_controller_query_status() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let status = controller
            .query_status()
            .await
            .expect("query_status failed");
        assert_eq!(status.model, PrinterModel::G2010);
        assert_eq!(status.serial_number, "UNKNOWN");
        assert_eq!(status.ink_levels.pigment_black_percent, 100);
        assert_eq!(status.waste_absorber_percent, 0.0);
    }

    #[tokio::test]
    async fn test_controller_thermal_emergency_abort_on_code_5200() {
        let mut mock = MockG2010Transport::new();
        mock.inject_support_code(Some("5200")); // Printhead overheated

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let config = PrintJobConfig::default();
        let line = ValidatedRasterLine::try_new(vec![255; 2550 * 3], 2550, 300, 3).unwrap();
        let pages = vec![vec![line]];

        let result = controller
            .submit_pwg_print_job(&config, &pages, "20261106180000")
            .await;

        match result {
            Err(DriverError::SafetyInterlock(SafetyInterlockError::EmergencyHardwareAbort {
                support_code,
                ..
            })) => {
                assert_eq!(support_code, "5200");
            }
            other => panic!("Expected EmergencyHardwareAbort for code 5200, got: {other:?}"),
        }

        // Verify CancelJob was sent to the printer
        assert!(controller
            .transport()
            .recorded_ivec_operations()
            .contains(&"CancelJob".to_string()));
    }

    #[tokio::test]
    async fn test_controller_thermal_emergency_abort_mid_job_page_two() {
        let mut mock = MockG2010Transport::new();
        // Inject 5200 after page 1 finishes
        mock.inject_support_code_after_pages("5200", 1);

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let config = PrintJobConfig::default();
        let line = ValidatedRasterLine::try_new(vec![255; 2550 * 3], 2550, 300, 3).unwrap();
        // 2 pages
        let pages = vec![vec![line.clone()], vec![line]];

        let result = controller
            .submit_pwg_print_job(&config, &pages, "20261106180000")
            .await;

        match result {
            Err(DriverError::SafetyInterlock(SafetyInterlockError::EmergencyHardwareAbort {
                support_code,
                ..
            })) => {
                assert_eq!(support_code, "5200");
            }
            other => {
                panic!("Expected EmergencyHardwareAbort for code 5200 on page 2, got: {other:?}")
            }
        }

        let ops = controller.transport().recorded_ivec_operations();
        // Page 1 was configured, but before page 2 streamed, CancelJob was issued!
        assert!(ops.contains(&"SetPageConfiguration".to_string()));
        assert!(ops.contains(&"CancelJob".to_string()));
        // EndJob was NOT reached
        assert!(!ops.contains(&"EndJob".to_string()));
    }

    #[tokio::test]
    async fn test_controller_maintenance_cooldown_and_authorization() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        // Standard clean succeeds
        let action = MaintenanceAction::Clean {
            kind: CleaningType::Regular,
            target: CleaningTarget::All,
        };
        let status = controller
            .execute_maintenance(action.clone(), None, false)
            .await
            .expect("First standard clean should succeed");
        assert_eq!(status.model, PrinterModel::G2010);

        // Immediate second clean is blocked by 60s cooldown
        let err = controller
            .execute_maintenance(action.clone(), None, false)
            .await
            .expect_err("Second clean should fail cooldown");
        match err {
            DriverError::SafetyInterlock(SafetyInterlockError::CooldownActive {
                operation,
                remaining_seconds,
            }) => {
                assert_eq!(operation, "Standard Cleaning");
                assert!(remaining_seconds > 0);
            }
            other => panic!("Expected CooldownActive, got {other:?}"),
        }

        // Ink flush without confirmation token is blocked
        let flush_action = MaintenanceAction::Clean {
            kind: CleaningType::InkFlush,
            target: CleaningTarget::All,
        };
        let flush_err = controller
            .execute_maintenance(flush_action.clone(), None, false)
            .await
            .expect_err("Ink flush without token should fail");
        match flush_err {
            DriverError::SafetyInterlock(SafetyInterlockError::ConfirmationRequired {
                expected_token,
            }) => {
                assert_eq!(expected_token, INK_FLUSH_CONFIRMATION_TOKEN);
            }
            other => panic!("Expected ConfirmationRequired, got {other:?}"),
        }

        // Ink flush with token succeeds
        let flush_ok = controller
            .execute_maintenance(flush_action, Some(INK_FLUSH_CONFIRMATION_TOKEN), false)
            .await
            .expect("Ink flush with token should succeed");
        assert_eq!(flush_ok.model, PrinterModel::G2010);
    }

    #[tokio::test]
    async fn test_controller_absorber_near_capacity_block() {
        let mut mock = MockG2010Transport::new();
        mock.set_waste_absorber_percent(85.0);

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        // Query status to update supervisor D-counter
        let _ = controller.query_status().await.unwrap();

        let flush_action = MaintenanceAction::Clean {
            kind: CleaningType::InkFlush,
            target: CleaningTarget::All,
        };

        let err = controller
            .execute_maintenance(flush_action, Some(INK_FLUSH_CONFIRMATION_TOKEN), false)
            .await
            .expect_err("Ink flush on >=80% absorber must be blocked!");

        match err {
            DriverError::SafetyInterlock(SafetyInterlockError::AbsorberNearCapacity {
                current_percent,
                max_safe_percent,
            }) => {
                assert!(current_percent >= 80.0);
                assert_eq!(max_safe_percent, 80.0);
            }
            other => panic!("Expected AbsorberNearCapacity, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn test_controller_submit_pwg_print_job_success() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let config = PrintJobConfig::default();
        let line = ValidatedRasterLine::try_new(vec![255; 2550 * 3], 2550, 300, 3).unwrap();
        let pages = vec![vec![line.clone(), line]];

        controller
            .submit_pwg_print_job(&config, &pages, "20261106180000")
            .await
            .expect("Print job execution should succeed");

        let ops = controller.transport().recorded_ivec_operations();
        assert!(ops.contains(&"SetJobConfiguration".to_string()));
        assert!(ops.contains(&"StartJob".to_string()));
        assert!(ops.contains(&"SendData".to_string()));
        assert!(ops.contains(&"SetPageConfiguration".to_string()));
        assert!(ops.contains(&"EndJob".to_string()));
    }

    #[tokio::test]
    async fn test_controller_acquire_raw_scan_success() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let params = ScanParameters {
            resolution: ScanResolution::Dpi300,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry {
                x_offset: 0,
                y_offset: 0,
                width: 300,
                height: 32,
            },
            ..Default::default()
        };

        let scan_bytes = controller
            .acquire_raw_scan(&params)
            .await
            .expect("Scan acquisition should succeed");

        let expected_bytes = 300 * 32 * 3;
        assert_eq!(scan_bytes.len(), expected_bytes);

        let ops = controller.transport().recorded_ivec_operations();
        assert!(ops.contains(&"ScanStartJob".to_string()));
        assert!(ops.contains(&"ScanModeShift".to_string()));
        assert!(ops.contains(&"ScanEndJob".to_string()));
    }

    #[tokio::test]
    async fn test_controller_mechanical_clean_and_test_print() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let mut controller = G2010Controller::new(mock, supervisor);

        let roller_res = controller
            .execute_maintenance(
                MaintenanceAction::MechanicalClean(RollerCleanTarget::FeedRoller),
                None,
                false,
            )
            .await;
        assert!(roller_res.is_ok());

        let test_print_res = controller
            .execute_maintenance(
                MaintenanceAction::TestPrint(TestPrintType::NozzleCheck),
                None,
                false,
            )
            .await;
        assert!(test_print_res.is_ok());
    }
}
