//! Integration tests for pixma-printer-app: IPP/2.0 binary requests and REST APIs over Axum.

use axum::body::to_bytes;
use axum::http::{header, Request, StatusCode};
use pixma_core::maintenance::{
    CleaningTarget, CleaningType, MaintenanceAction, MaintenanceRecord, MaintenanceSupervisor,
};
use pixma_core::models::DeviceStatus;
use pixma_daemon::DaemonClient;
use pixma_printer_app::ipp::{
    IppRequest, OP_GET_JOBS, OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB, OP_VALIDATE_JOB,
    STATUS_SUCCESSFUL_OK, TAG_CHARSET, TAG_END_OF_ATTRIBUTES, TAG_NATURAL_LANGUAGE,
    TAG_OPERATION_ATTRIBUTES,
};
use pixma_printer_app::router::{build_router, AppState, MaintenanceApiRequest};
use pixma_raster::cups_reader::RasterPageInput;
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use tower::ServiceExt;

fn create_test_state() -> AppState {
    let mock = MockG2010Transport::new();
    let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
    let controller = G2010Controller::new(AnyTransport::Mock(Box::new(mock)), supervisor);
    let client = DaemonClient::direct(controller);
    AppState::new(client)
}

fn build_raw_ipp_request(op_id: u16, req_id: u32, data: Option<&[u8]>) -> Vec<u8> {
    let mut buf = Vec::new();
    // IPP/2.0 version
    buf.push(2);
    buf.push(0);
    // Operation ID
    buf.extend_from_slice(&op_id.to_be_bytes());
    // Request ID
    buf.extend_from_slice(&req_id.to_be_bytes());

    // Group 1: Operation attributes
    buf.push(TAG_OPERATION_ATTRIBUTES);
    // attributes-charset
    buf.push(TAG_CHARSET);
    let name = "attributes-charset";
    buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
    buf.extend_from_slice(name.as_bytes());
    let val = "utf-8";
    buf.extend_from_slice(&(val.len() as u16).to_be_bytes());
    buf.extend_from_slice(val.as_bytes());

    // attributes-natural-language
    buf.push(TAG_NATURAL_LANGUAGE);
    let name = "attributes-natural-language";
    buf.extend_from_slice(&(name.len() as u16).to_be_bytes());
    buf.extend_from_slice(name.as_bytes());
    let val = "en-us";
    buf.extend_from_slice(&(val.len() as u16).to_be_bytes());
    buf.extend_from_slice(val.as_bytes());

    // End delimiter
    buf.push(TAG_END_OF_ATTRIBUTES);

    // Optional document data
    if let Some(d) = data {
        buf.extend_from_slice(d);
    }

    buf
}

#[tokio::test]
async fn test_axum_get_printer_attributes() {
    let app = build_router(create_test_state());
    let req_body = build_raw_ipp_request(OP_GET_PRINTER_ATTRIBUTES, 1, None);

    let req = Request::builder()
        .method("POST")
        .uri("/ipp/print")
        .header(header::CONTENT_TYPE, "application/ipp")
        .body(axum::body::Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/ipp"
    );

    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let parsed = IppRequest::parse(&body_bytes).expect("Failed to parse IPP response");

    assert_eq!(parsed.version_major, 2);
    assert_eq!(parsed.version_minor, 0);
    assert_eq!(parsed.operation_id, STATUS_SUCCESSFUL_OK);
    assert_eq!(parsed.request_id, 1);

    // Verify required printer attributes
    let p_name = parsed
        .printer_attributes
        .iter()
        .find(|a| a.name() == "printer-name")
        .expect("missing printer-name");
    assert_eq!(
        p_name.first_value().unwrap().as_str(),
        Some("Canon PIXMA G2010")
    );

    let p_state = parsed
        .printer_attributes
        .iter()
        .find(|a| a.name() == "printer-state")
        .expect("missing printer-state");
    assert_eq!(p_state.first_value().unwrap().as_i32(), Some(3)); // idle

    let doc_formats = parsed
        .printer_attributes
        .iter()
        .find(|a| a.name() == "document-format-supported")
        .expect("missing document-format-supported");
    let formats: Vec<&str> = doc_formats
        .values
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert!(formats.contains(&"image/pwg-raster"));
    assert!(formats.contains(&"image/png"));
    assert!(formats.contains(&"image/jpeg"));

    let marker_names = parsed
        .printer_attributes
        .iter()
        .find(|a| a.name() == "marker-names")
        .expect("missing marker-names");
    assert_eq!(marker_names.values.len(), 4);

    let marker_levels = parsed
        .printer_attributes
        .iter()
        .find(|a| a.name() == "marker-levels")
        .expect("missing marker-levels");
    assert_eq!(marker_levels.values.len(), 4);
    assert_eq!(marker_levels.values[0].as_i32(), Some(100));
}

#[tokio::test]
async fn test_axum_validate_job() {
    let app = build_router(create_test_state());
    let req_body = build_raw_ipp_request(OP_VALIDATE_JOB, 42, None);

    let req = Request::builder()
        .method("POST")
        .uri("/ipp/print")
        .header(header::CONTENT_TYPE, "application/ipp")
        .body(axum::body::Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let parsed = IppRequest::parse(&body_bytes).expect("Failed to parse IPP response");
    assert_eq!(parsed.operation_id, STATUS_SUCCESSFUL_OK);
    assert_eq!(parsed.request_id, 42);
}

#[tokio::test]
async fn test_axum_print_job() {
    let app = build_router(create_test_state());

    // Create a 100x100 dummy PNG image
    let page = RasterPageInput::new_blank(100, 100, 300);
    let mut png_bytes = Vec::new();
    {
        use image::ImageEncoder;
        let encoder = image::codecs::png::PngEncoder::new(&mut png_bytes);
        encoder
            .write_image(&page.rgb_pixels, 100, 100, image::ExtendedColorType::Rgb8)
            .unwrap();
    }

    let req_body = build_raw_ipp_request(OP_PRINT_JOB, 7, Some(&png_bytes));

    let req = Request::builder()
        .method("POST")
        .uri("/ipp/print")
        .header(header::CONTENT_TYPE, "application/ipp")
        .body(axum::body::Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let parsed = IppRequest::parse(&body_bytes).expect("Failed to parse IPP response");

    assert_eq!(parsed.operation_id, STATUS_SUCCESSFUL_OK);
    assert_eq!(parsed.request_id, 7);

    let job_id_attr = parsed
        .job_attributes
        .iter()
        .find(|a| a.name() == "job-id")
        .expect("missing job-id attribute");
    assert!(job_id_attr.first_value().unwrap().as_i32().unwrap() >= 1);
}

#[tokio::test]
async fn test_axum_get_jobs() {
    let app = build_router(create_test_state());
    let req_body = build_raw_ipp_request(OP_GET_JOBS, 99, None);

    let req = Request::builder()
        .method("POST")
        .uri("/ipp/print")
        .header(header::CONTENT_TYPE, "application/ipp")
        .body(axum::body::Body::from(req_body))
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
    let parsed = IppRequest::parse(&body_bytes).expect("Failed to parse IPP response");
    assert_eq!(parsed.operation_id, STATUS_SUCCESSFUL_OK);
    assert_eq!(parsed.request_id, 99);
}

#[tokio::test]
async fn test_axum_rest_status_and_maintenance() {
    let app = build_router(create_test_state());

    // 1. GET /api/status
    let status_req = Request::builder()
        .method("GET")
        .uri("/api/status")
        .body(axum::body::Body::empty())
        .unwrap();

    let status_resp = app.clone().oneshot(status_req).await.unwrap();
    assert_eq!(status_resp.status(), StatusCode::OK);

    let body_bytes = to_bytes(status_resp.into_body(), usize::MAX).await.unwrap();
    let status: DeviceStatus = serde_json::from_slice(&body_bytes).unwrap();
    assert_eq!(status.serial_number, "UNKNOWN");

    // 2. POST /api/maintenance (Standard Clean)
    let maint_req_body = MaintenanceApiRequest {
        action: MaintenanceAction::Clean {
            kind: CleaningType::Regular,
            target: CleaningTarget::All,
        },
        confirmation_token: None,
        override_cooldown: false,
    };
    let json_body = serde_json::to_vec(&maint_req_body).unwrap();

    let maint_req = Request::builder()
        .method("POST")
        .uri("/api/maintenance")
        .header(header::CONTENT_TYPE, "application/json")
        .body(axum::body::Body::from(json_body))
        .unwrap();

    let maint_resp = app.oneshot(maint_req).await.unwrap();
    assert_eq!(maint_resp.status(), StatusCode::OK);

    let maint_bytes = to_bytes(maint_resp.into_body(), usize::MAX).await.unwrap();
    let maint_status: DeviceStatus = serde_json::from_slice(&maint_bytes).unwrap();
    assert_eq!(maint_status.serial_number, "UNKNOWN");
}
