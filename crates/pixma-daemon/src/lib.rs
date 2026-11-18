//! pixmad background USB multiplexer, resource arbitrator, and Unix domain socket broker.

pub mod client;
pub mod ipc;
pub mod server;

pub use client::{
    current_uid, default_socket_candidates, ensure_verified_private_dir, is_verified_private_dir,
    DaemonClient,
};
pub use ipc::{
    local_hostname, read_frame_cbor, read_frame_json, recv_request, recv_response, send_request,
    send_response, write_frame_cbor, write_frame_json, ConnectionSource, DaemonRequest,
    DaemonResponse, DeviceArbitrator, JobLockGuard, JobLockKind, LiveDaemonSnapshot,
    MAX_IPC_CBOR_BYTES, MAX_IPC_JSON_BYTES,
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

    #[tokio::test]
    async fn test_live_snapshot_idle_and_busy_printing_lock() {
        let socket_dir = std::env::temp_dir();
        let socket_path =
            socket_dir.join(format!("pixmad_snapshot_test_{}.sock", std::process::id()));

        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);

        let server = Arc::new(DaemonServer::bind(&socket_path, controller).expect("Bind failed"));
        let arbitrator = Arc::clone(server.arbitrator());
        let server_runner = Arc::clone(&server);
        let server_task = tokio::spawn(async move {
            let _ = server_runner.run().await;
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(20)).await;

        let mut client = DaemonClient::connect_live(Some(&socket_path), None)
            .await
            .expect("Live socket connect failed");

        // 1. Busy without cache while Printing lock is held must return non-blocking Printing
        let print_guard = arbitrator.acquire(JobLockKind::Printing).await;
        let busy_snap = tokio::time::timeout(
            tokio::time::Duration::from_millis(200),
            client.get_live_snapshot(),
        )
        .await
        .expect("GetLiveSnapshot must not block during print job")
        .expect("Busy snapshot failed");
        assert_eq!(busy_snap.active_job, Some(JobLockKind::Printing));
        assert!(busy_snap.telemetry_cached_during_job);
        assert_eq!(busy_snap.status.state, OperationalState::Printing);
        drop(print_guard);

        // 2. Idle live snapshot queries hardware status, port byte, IEEE-1284 ID, and host
        let idle_snap = client.get_live_snapshot().await.expect("Idle snapshot failed");
        assert_eq!(idle_snap.active_job, None);
        assert!(!idle_snap.telemetry_cached_during_job);
        assert_eq!(idle_snap.daemon_hostname, local_hostname());
        assert_eq!(idle_snap.usb_port_status, Some(0x18));
        assert!(idle_snap.ieee1284_id.is_some());
        assert!(idle_snap.hardware_marker.is_mock);

        server_task.abort();
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn test_connect_live_does_not_fallback_to_mock_when_socket_missing() {
        let missing = std::env::temp_dir().join(format!(
            "pixmad_missing_socket_{}.sock",
            std::process::id()
        ));
        let err = DaemonClient::connect_live(Some(&missing), None)
            .await
            .err()
            .expect("connect_live must error when explicit socket does not exist");
        assert!(matches!(err, pixma_core::errors::DriverError::Transport(_)));
    }

    #[tokio::test]
    async fn test_cbor_raw_bytes_and_json_compat_roundtrip() {
        // 300/600/1200 DPI representative buffers up to 24 MiB PrintRawStream
        let print_data: Vec<u8> = (0..(24 * 1024 * 1024))
            .map(|i| (i % 251) as u8)
            .collect();
        let req = DaemonRequest::PrintRawStream {
            data: print_data.clone(),
        };

        let mut frame = Vec::new();
        send_request(&mut frame, &req).await.unwrap();
        // Verify compact binary CBOR framing (~1.00x payload, zero 4x JSON overhead)
        assert!(
            frame.len() <= print_data.len() + 256,
            "CBOR frame size ({}) bloated vs raw bytes ({})",
            frame.len(),
            print_data.len()
        );
        let decoded = recv_request(&mut &frame[..]).await.unwrap();
        assert_eq!(decoded, req);

        // High-DPI ScanData Ok & Err over send_response/recv_response
        let scan_payload: Vec<u8> = (0..(12 * 1024 * 1024))
            .map(|i| ((i * 7) % 256) as u8)
            .collect();
        let scan_ok = DaemonResponse::ScanData(Ok(scan_payload.clone()));
        let mut resp_frame = Vec::new();
        send_response(&mut resp_frame, &scan_ok).await.unwrap();
        assert!(resp_frame.len() <= scan_payload.len() + 256);
        assert_eq!(recv_response(&mut &resp_frame[..]).await.unwrap(), scan_ok);

        let scan_err = DaemonResponse::ScanData(Err("Scanner sensor busy".to_string()));
        let mut err_frame = Vec::new();
        send_response(&mut err_frame, &scan_err).await.unwrap();
        assert_eq!(recv_response(&mut &err_frame[..]).await.unwrap(), scan_err);

        // Verify JSON wire compatibility and legacy sequence-of-u8 decoding
        let legacy_json = br#"{"ScanData":{"Ok":[1,2,3,255]}}"#;
        let mut legacy_frame = Vec::new();
        legacy_frame.extend_from_slice(&(legacy_json.len() as u32).to_be_bytes());
        legacy_frame.extend_from_slice(legacy_json);
        let parsed: DaemonResponse = read_frame_json(&mut &legacy_frame[..]).await.unwrap();
        assert_eq!(parsed, DaemonResponse::ScanData(Ok(vec![1, 2, 3, 255])));
    }

    #[tokio::test]
    async fn test_ipc_frame_bounds_reject_oversized_on_read_and_write() {
        let too_big_json_hdr = ((MAX_IPC_JSON_BYTES as u32) + 1).to_be_bytes();
        let err = read_frame_json::<_, DaemonResponse>(&mut &too_big_json_hdr[..])
            .await
            .expect_err("Oversized JSON header must fail before allocation");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        let too_big_cbor_hdr = ((MAX_IPC_CBOR_BYTES as u32) + 1).to_be_bytes();
        let err = read_frame_cbor::<_, DaemonRequest>(&mut &too_big_cbor_hdr[..])
            .await
            .expect_err("Oversized CBOR header must fail before allocation");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);

        let oversized_json_msg =
            DaemonResponse::Error("x".repeat(MAX_IPC_JSON_BYTES + 128));
        let mut sink = Vec::new();
        let write_err = write_frame_json(&mut sink, &oversized_json_msg)
            .await
            .expect_err("Oversized JSON write must be rejected");
        assert_eq!(write_err.kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_daemon_socket_permissions_and_connect_mock() {
        use std::os::unix::fs::PermissionsExt;

        let socket_path = std::env::temp_dir().join(format!(
            "pixmad_perm_test_{}.sock",
            std::process::id()
        ));
        let mock = MockG2010Transport::new();
        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
        let server = DaemonServer::bind(&socket_path, controller).expect("Bind failed");
        let mode = std::fs::metadata(&socket_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o660);
        drop(server);

        let mut client = DaemonClient::connect_mock(None).expect("connect_mock failed");
        assert_eq!(client.source(), &ConnectionSource::DirectMock);
        client.ping().await.unwrap();
        let snap = client.get_live_snapshot().await.unwrap();
        assert!(snap.hardware_marker.is_mock);
        let caps = client.get_maintenance_capabilities().await.unwrap();
        assert_eq!(caps.supports("regular", "group2"), Some(true));
        assert_eq!(caps.supports("deep", "group2"), Some(false));
    }

    #[tokio::test]
    async fn test_get_maintenance_capabilities_maps_eof_from_legacy_daemon() {
        let sock_path = std::env::temp_dir().join(format!(
            "pixmad_legacy_eof_test_{}.sock",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&sock_path);
        let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();
        let server_task = tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                // Immediately drop connection as a v0.3.0 pixmad does upon unknown CBOR variant
                drop(stream);
            }
        });

        let mut client = DaemonClient::connect_live(Some(&sock_path), None)
            .await
            .expect("Must connect to test socket");
        let err = client
            .get_maintenance_capabilities()
            .await
            .expect_err("Legacy daemon closing socket must map cleanly");
        assert!(
            err.to_string().contains("older than v0.3.1"),
            "unexpected err: {err}"
        );
        let _ = server_task.await;
        let _ = std::fs::remove_file(&sock_path);
    }
}
