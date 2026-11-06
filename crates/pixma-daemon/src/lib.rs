//! pixmad background USB multiplexer, resource arbitrator, and Unix domain socket broker.

pub mod client;
pub mod ipc;
pub mod server;

pub use client::DaemonClient;
pub use ipc::{
    read_frame_cbor, read_frame_json, recv_request, recv_response, send_request, send_response,
    write_frame_cbor, write_frame_json, DaemonRequest, DaemonResponse, DeviceArbitrator,
    JobLockGuard, JobLockKind,
};
pub use server::DaemonServer;

#[cfg(test)]
mod tests {
    use super::*;
    use pixma_core::maintenance::{
        CleaningTarget, CleaningType, MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor,
        INK_FLUSH_CONFIRMATION_TOKEN,
    };
    use pixma_core::models::{DeviceStatus, OperationalState, PrinterModel};
    use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
    use pixma_transport::controller::{AnyTransport, G2010Controller};
    use pixma_transport::mock_device::MockG2010Transport;
    use std::sync::Arc;

    #[tokio::test]
    async fn test_cbor_and_json_serialization_roundtrip() {
        let req = DaemonRequest::ExecuteMaintenance {
            action: MaintenanceAction::Clean {
                kind: CleaningType::InkFlush,
                target: CleaningTarget::All,
            },
            confirmation_token: Some(INK_FLUSH_CONFIRMATION_TOKEN.to_string()),
            override_cooldown: false,
        };

        // JSON roundtrip
        let mut json_buf = Vec::new();
        write_frame_json(&mut json_buf, &req).await.unwrap();
        let decoded_json: DaemonRequest = read_frame_json(&mut &json_buf[..]).await.unwrap();
        assert_eq!(req, decoded_json);

        // CBOR roundtrip
        let mut cbor_buf = Vec::new();
        write_frame_cbor(&mut cbor_buf, &req).await.unwrap();
        let decoded_cbor: DaemonRequest = read_frame_cbor(&mut &cbor_buf[..]).await.unwrap();
        assert_eq!(req, decoded_cbor);

        // Response roundtrip
        let resp = DaemonResponse::Status(DeviceStatus {
            model: PrinterModel::G2010,
            state: OperationalState::Idle,
            status_detail: "Idle".to_string(),
            waste_absorber_percent: 12.5,
            ..Default::default()
        });

        let mut resp_json = Vec::new();
        write_frame_json(&mut resp_json, &resp).await.unwrap();
        let decoded_resp: DaemonResponse = read_frame_json(&mut &resp_json[..]).await.unwrap();
        assert_eq!(resp, decoded_resp);
    }

    #[tokio::test]
    async fn test_device_arbitrator_mutual_exclusion() {
        let arbitrator = DeviceArbitrator::new();

        let guard1 = arbitrator.acquire(JobLockKind::Printing).await;
        assert_eq!(guard1.kind(), JobLockKind::Printing);

        // Second acquisition will block until guard1 is dropped
        let arbitrator_clone = Arc::clone(&arbitrator);
        let handle = tokio::spawn(async move {
            let guard2 = arbitrator_clone.acquire(JobLockKind::Scanning).await;
            assert_eq!(guard2.kind(), JobLockKind::Scanning);
            42
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;
        assert!(!handle.is_finished());

        drop(guard1);
        let res = handle.await.unwrap();
        assert_eq!(res, 42);
    }

    #[tokio::test]
    async fn test_daemon_unix_socket_ipc_roundtrip() {
        let socket_dir = std::env::temp_dir();
        let socket_path = socket_dir.join(format!("pixmad_test_{}.sock", std::process::id()));

        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);

        let server = DaemonServer::bind(&socket_path, controller).expect("Bind server failed");
        let server_task = tokio::spawn(async move {
            let _ = server.run().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let mut client = DaemonClient::connect_socket(&socket_path)
            .await
            .expect("Client connect failed");

        // 1. Ping
        client.ping().await.expect("Ping failed");

        // 2. GetStatus
        let status = client.get_status().await.expect("GetStatus failed");
        assert_eq!(status.model, PrinterModel::G2010);
        assert_eq!(status.waste_absorber_percent, 0.0);

        // 3. ExecuteMaintenance (Standard Clean)
        let action = MaintenanceAction::Clean {
            kind: CleaningType::Regular,
            target: CleaningTarget::All,
        };
        let maint_status = client
            .execute_maintenance(action.clone(), None, false)
            .await
            .expect("Maintenance execution failed");
        assert_eq!(maint_status.model, PrinterModel::G2010);

        // Immediate repeat must return CooldownActive error over IPC
        let repeat_err = client
            .execute_maintenance(action, None, false)
            .await
            .expect_err("Repeat clean must return cooldown error");
        match repeat_err {
            pixma_core::errors::DriverError::Protocol(msg) => {
                assert!(msg.contains("Safety cooldown active"));
            }
            other => panic!("Expected Protocol error with cooldown message, got: {other:?}"),
        }

        // 4. PrintRawStream
        let raw_print_data = b"TEST_PWG_RASTER_DATA";
        client
            .print_raw_stream(raw_print_data)
            .await
            .expect("Print raw stream failed");

        // 5. AcquireScan
        let scan_params = ScanParameters {
            resolution: ScanResolution::Dpi300,
            color_mode: ScanColorMode::Color24Bit,
            geometry: ScanGeometry {
                x_offset: 0,
                y_offset: 0,
                width: 100,
                height: 16,
            },
            ..Default::default()
        };
        let scan_data = client
            .acquire_scan(&scan_params)
            .await
            .expect("AcquireScan failed");
        assert_eq!(scan_data.len(), 100 * 16 * 3);

        server_task.abort();
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn test_daemon_client_direct_fallback() {
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);

        let mut client = DaemonClient::direct(controller);

        client.ping().await.unwrap();
        let status = client.get_status().await.unwrap();
        assert_eq!(status.model, PrinterModel::G2010);
    }
}
