//! eSCL (AirScan) HTTP/REST microserver implementation using Axum.
//!
//! Exposes driverless network scanner endpoints conforming to Apple AirScan (eSCL)
//! and PWG Scan Service specifications:
//! - `GET /eSCL/ScannerCapabilities`
//! - `GET /eSCL/ScannerStatus`
//! - `POST /eSCL/ScanJobs`
//! - `GET /eSCL/ScanJobs/{uuid}/NextDocument`
//! - `DELETE /eSCL/ScanJobs/{uuid}`

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, State};
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use image::RgbImage;
use pixma_core::maintenance::{MaintenanceRecord, MaintenanceSupervisor};
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_imageproc::{export_jpeg, export_pdf_multipage, export_png};
use pixma_transport::controller::{AnyTransport, G2010Controller};
use pixma_transport::mock_device::MockG2010Transport;
use pixma_transport::nusb_transport::{discover_canon_devices, NusbTransport};
use tokio::sync::{Mutex, RwLock};

use crate::xml_models::{
    build_scanner_capabilities_xml, build_scanner_status_xml, parse_scan_settings_xml, EsclJobInfo,
};

static JOB_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generates a compliant unique UUID string for each scan job.
pub fn generate_job_uuid() -> String {
    let id = JOB_COUNTER.fetch_add(1, Ordering::SeqCst);
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    format!("{now:08x}-a001-4000-8000-{id:012x}")
}

/// In-memory stored document and delivery state for an eSCL scan job.
#[derive(Debug, Clone)]
pub struct StoredScanJob {
    pub uuid: String,
    pub content_type: String,
    pub document_bytes: Vec<u8>,
    pub created_at: Instant,
    pub retrieved: bool,
}

/// Shared application state for the eSCL server.
pub struct EsclServerState {
    pub controller: Mutex<G2010Controller<AnyTransport>>,
    pub jobs: RwLock<HashMap<String, StoredScanJob>>,
}

impl EsclServerState {
    /// Creates a new server state, choosing mock or physical USB device.
    pub fn new(mock: bool) -> Self {
        let is_mock_forced = mock || std::env::var("PIXMA_MOCK").as_deref() == Ok("1");
        let transport = if is_mock_forced {
            AnyTransport::Mock(Box::new(MockG2010Transport::new()))
        } else if let Ok(devs) = discover_canon_devices() {
            if !devs.is_empty() {
                match NusbTransport::open_first() {
                    Ok(u) => AnyTransport::Usb(u),
                    Err(e) => {
                        tracing::warn!("Failed to open physical USB: {e}; using mock");
                        AnyTransport::Mock(Box::new(MockG2010Transport::new()))
                    }
                }
            } else {
                AnyTransport::Mock(Box::new(MockG2010Transport::new()))
            }
        } else {
            AnyTransport::Mock(Box::new(MockG2010Transport::new()))
        };

        let supervisor = MaintenanceSupervisor::in_memory(MaintenanceRecord::default());
        let controller = G2010Controller::new(transport, supervisor);

        Self {
            controller: Mutex::new(controller),
            jobs: RwLock::new(HashMap::new()),
        }
    }

    /// Creates an in-memory mock server state for testing.
    pub fn new_mock() -> Self {
        Self::new(true)
    }
}

/// Handles `GET /eSCL/ScannerCapabilities`.
async fn handle_capabilities() -> Response {
    let xml = build_scanner_capabilities_xml();
    ([(CONTENT_TYPE, "text/xml; charset=utf-8")], xml).into_response()
}

/// Handles `GET /eSCL/ScannerStatus`.
async fn handle_status(State(state): State<Arc<EsclServerState>>) -> Response {
    let jobs_guard = state.jobs.read().await;
    let mut job_infos = Vec::new();
    let mut is_processing = false;

    for job in jobs_guard.values() {
        let age_seconds = job.created_at.elapsed().as_secs() as u32;
        let (job_state, reason) = if job.retrieved {
            ("Completed", "JobCompletedSuccessfully")
        } else {
            is_processing = true;
            ("Processing", "JobScanning")
        };

        job_infos.push(EsclJobInfo {
            uuid: job.uuid.clone(),
            age_seconds,
            state: job_state.to_string(),
            reason: reason.to_string(),
        });
    }

    let scanner_state = if is_processing { "Processing" } else { "Idle" };
    let xml = build_scanner_status_xml(scanner_state, &job_infos);
    ([(CONTENT_TYPE, "text/xml; charset=utf-8")], xml).into_response()
}

/// Handles `POST /eSCL/ScanJobs`.
async fn handle_create_scan_job(
    State(state): State<Arc<EsclServerState>>,
    body: String,
) -> Result<Response, (StatusCode, String)> {
    let req = parse_scan_settings_xml(&body).map_err(|e| (StatusCode::BAD_REQUEST, e))?;

    let resolution = ScanResolution::from_dpi(req.x_resolution).unwrap_or(ScanResolution::Dpi300);
    let color_mode = match req.color_mode.as_str() {
        "Grayscale8" => ScanColorMode::Grayscale8Bit,
        "BlackAndWhite1" => ScanColorMode::Lineart1Bit,
        _ => ScanColorMode::Color24Bit,
    };

    // eSCL scan regions are specified in 300ths of an inch (1/300")
    let target_dpi = resolution.as_dpi() as u32;
    let (max_w, max_h) = resolution.max_flatbed_pixels();

    let x_offset = ((req.x_offset.unwrap_or(0) * target_dpi) / 300).min(max_w.saturating_sub(1));
    let y_offset = ((req.y_offset.unwrap_or(0) * target_dpi) / 300).min(max_h.saturating_sub(1));
    let width =
        ((req.width.unwrap_or(2550) * target_dpi) / 300).clamp(1, max_w.saturating_sub(x_offset));
    let height =
        ((req.height.unwrap_or(3508) * target_dpi) / 300).clamp(1, max_h.saturating_sub(y_offset));

    let geometry = ScanGeometry {
        x_offset,
        y_offset,
        width,
        height,
    };

    let params = ScanParameters {
        resolution,
        color_mode,
        geometry,
        auto_multi_crop: false,
        deskew: false,
        descreen_moire: false,
        unsharp_mask: true,
        remove_dust_scratches: false,
        correct_gutter_shadow: false,
        gamma: 2.2,
    };

    let raw_bytes = {
        let mut ctrl = state.controller.lock().await;
        ctrl.acquire_raw_scan(&params)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    // Reconstruct RGB image
    let bpp = color_mode.bytes_per_pixel();
    let expected_len = (width as usize) * (height as usize) * bpp;
    if raw_bytes.len() < expected_len {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            format!(
                "Scan payload truncated: {} < {}",
                raw_bytes.len(),
                expected_len
            ),
        ));
    }

    let rgb_img = if bpp == 3 {
        RgbImage::from_raw(width, height, raw_bytes[..expected_len].to_vec()).ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid RGB raster buffer".to_string(),
        ))?
    } else {
        let mut expanded = Vec::with_capacity((width * height * 3) as usize);
        for &g in &raw_bytes[..expected_len] {
            expanded.extend_from_slice(&[g, g, g]);
        }
        RgbImage::from_raw(width, height, expanded).ok_or((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Invalid Gray raster buffer".to_string(),
        ))?
    };

    let processed = pixma_imageproc::execute_scan_pipeline(rgb_img, &params).map_err(|e| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Pipeline error: {e:?}"),
        )
    })?;

    if processed.is_empty() {
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            "Empty pipeline output".to_string(),
        ));
    }

    // Document encoding
    let (content_type, doc_bytes) = match req.document_format.as_str() {
        "image/png" => {
            let bytes = export_png(&processed[0])
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            ("image/png", bytes)
        }
        "image/jpeg" => {
            let bytes = export_jpeg(&processed[0], 90)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            ("image/jpeg", bytes)
        }
        _ => {
            // Default: application/pdf
            let bytes = export_pdf_multipage(&processed, target_dpi)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            ("application/pdf", bytes)
        }
    };

    let uuid = generate_job_uuid();
    let location = format!("/eSCL/ScanJobs/{uuid}");

    let job = StoredScanJob {
        uuid: uuid.clone(),
        content_type: content_type.to_string(),
        document_bytes: doc_bytes,
        created_at: Instant::now(),
        retrieved: false,
    };

    {
        let mut jobs_guard = state.jobs.write().await;
        jobs_guard.insert(uuid, job);
    }

    let mut headers = HeaderMap::new();
    headers.insert(
        LOCATION,
        HeaderValue::from_str(&location).map_err(|_| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Invalid location header".to_string(),
            )
        })?,
    );

    Ok((StatusCode::CREATED, headers, ()).into_response())
}

/// Handles `GET /eSCL/ScanJobs/{uuid}/NextDocument`.
///
/// Returns 200 OK with document bytes on first retrieval, then marks delivered
/// so subsequent calls return 404 Not Found per eSCL spec!
async fn handle_next_document(
    State(state): State<Arc<EsclServerState>>,
    Path(uuid): Path<String>,
) -> Response {
    let mut jobs_guard = state.jobs.write().await;
    let job = match jobs_guard.get_mut(&uuid) {
        Some(j) => j,
        None => return (StatusCode::NOT_FOUND, "Job not found").into_response(),
    };

    if job.retrieved {
        return (StatusCode::NOT_FOUND, "Document already retrieved").into_response();
    }

    job.retrieved = true;
    let mime = job.content_type.clone();
    let body = job.document_bytes.clone();

    ([(CONTENT_TYPE, mime)], body).into_response()
}

/// Handles `DELETE /eSCL/ScanJobs/{uuid}`.
async fn handle_delete_scan_job(
    State(state): State<Arc<EsclServerState>>,
    Path(uuid): Path<String>,
) -> Response {
    let mut jobs_guard = state.jobs.write().await;
    if jobs_guard.remove(&uuid).is_some() {
        StatusCode::OK.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    }
}

/// Builds the Axum router with all eSCL endpoints registered.
pub fn create_escl_router(state: Arc<EsclServerState>) -> Router {
    Router::new()
        .route("/eSCL/ScannerCapabilities", get(handle_capabilities))
        .route("/eSCL/ScannerStatus", get(handle_status))
        .route("/eSCL/ScanJobs", post(handle_create_scan_job))
        .route(
            "/eSCL/ScanJobs/:uuid/NextDocument",
            get(handle_next_document),
        )
        .route("/eSCL/ScanJobs/:uuid", delete(handle_delete_scan_job))
        .with_state(state)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    #[tokio::test]
    async fn test_escl_capabilities_route() {
        let state = Arc::new(EsclServerState::new_mock());
        let app = create_escl_router(state);

        let req = Request::builder()
            .uri("/eSCL/ScannerCapabilities")
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);
        let content_type = resp.headers().get(CONTENT_TYPE).unwrap().to_str().unwrap();
        assert!(content_type.contains("text/xml"));

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&bytes);
        assert!(body_str.contains("Canon PIXMA G2010 series"));
        assert!(body_str.contains("<scan:MaxWidth>2550</scan:MaxWidth>"));
    }

    #[tokio::test]
    async fn test_escl_status_route() {
        let state = Arc::new(EsclServerState::new_mock());
        let app = create_escl_router(state);

        let req = Request::builder()
            .uri("/eSCL/ScannerStatus")
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::OK);

        let bytes = to_bytes(resp.into_body(), usize::MAX).await.unwrap();
        let body_str = String::from_utf8_lossy(&bytes);
        assert!(body_str.contains("<pwg:State>Idle</pwg:State>"));
    }

    #[tokio::test]
    async fn test_escl_scan_job_lifecycle() {
        let state = Arc::new(EsclServerState::new_mock());
        let app = create_escl_router(Arc::clone(&state));

        // 1. Create a ScanJob with PNG format and small region
        let scan_settings = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <scan:ColorMode>RGB24</scan:ColorMode>
  <scan:XResolution>75</scan:XResolution>
  <scan:YResolution>75</scan:YResolution>
  <scan:DocumentFormat>image/png</scan:DocumentFormat>
  <pwg:ScanRegions>
    <pwg:ScanRegion>
      <pwg:XOffset>0</pwg:XOffset>
      <pwg:YOffset>0</pwg:YOffset>
      <pwg:Width>300</pwg:Width>
      <pwg:Height>300</pwg:Height>
    </pwg:ScanRegion>
  </pwg:ScanRegions>
</scan:ScanSettings>"#;

        let req = Request::builder()
            .uri("/eSCL/ScanJobs")
            .method("POST")
            .header("Content-Type", "text/xml")
            .body(Body::from(scan_settings))
            .unwrap();

        let resp = app.clone().oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);

        let location = resp
            .headers()
            .get(LOCATION)
            .expect("Location header missing")
            .to_str()
            .unwrap();
        assert!(location.starts_with("/eSCL/ScanJobs/"));

        let next_doc_uri = format!("{location}/NextDocument");

        // 2. Fetch document first time -> 200 OK with PNG
        let req_doc1 = Request::builder()
            .uri(&next_doc_uri)
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let resp_doc1 = app.clone().oneshot(req_doc1).await.unwrap();
        assert_eq!(resp_doc1.status(), StatusCode::OK);
        let ct = resp_doc1
            .headers()
            .get(CONTENT_TYPE)
            .unwrap()
            .to_str()
            .unwrap();
        assert_eq!(ct, "image/png");

        let doc_bytes = to_bytes(resp_doc1.into_body(), usize::MAX).await.unwrap();
        // PNG magic bytes
        assert_eq!(&doc_bytes[..4], b"\x89PNG");

        // 3. Fetch document second time -> 404 Not Found (eSCL spec requirement!)
        let req_doc2 = Request::builder()
            .uri(&next_doc_uri)
            .method("GET")
            .body(Body::empty())
            .unwrap();

        let resp_doc2 = app.clone().oneshot(req_doc2).await.unwrap();
        assert_eq!(resp_doc2.status(), StatusCode::NOT_FOUND);

        // 4. DELETE job -> 200 OK
        let req_del = Request::builder()
            .uri(location)
            .method("DELETE")
            .body(Body::empty())
            .unwrap();

        let resp_del = app.oneshot(req_del).await.unwrap();
        assert_eq!(resp_del.status(), StatusCode::OK);
    }
}
