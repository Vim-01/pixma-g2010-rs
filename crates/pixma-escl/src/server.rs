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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::header::{CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::Router;
use pixma_core::scan_params::{ScanColorMode, ScanGeometry, ScanParameters, ScanResolution};
use pixma_daemon::DaemonClient;
use pixma_imageproc::{decode_raw_scan_buffer, export_jpeg, export_pdf_multipage, export_png};
use tokio::sync::{Mutex, RwLock};

use crate::xml_models::{
    build_scanner_capabilities_xml, build_scanner_status_xml, parse_scan_settings_xml, EsclJobInfo,
};

const MAX_RETAINED_JOBS: usize = 64;
const MAX_ESCL_XML_BODY_BYTES: usize = 1024 * 1024;
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
    pub is_mock: bool,
    client: Mutex<Option<DaemonClient>>,
    socket_path: Option<PathBuf>,
    pub jobs: RwLock<HashMap<String, StoredScanJob>>,
}

impl EsclServerState {
    /// Creates a new server state. In non-mock mode, connects via `pixmad` / USB on-demand
    /// per scan job so the USB interface and session lock are never held while idle.
    pub fn new(mock: bool) -> Self {
        Self::with_socket(mock, None)
    }

    /// Creates a new server state targeting a specific `pixmad` socket path when live.
    pub fn with_socket(mock: bool, socket_path: Option<PathBuf>) -> Self {
        let is_mock = mock || std::env::var("PIXMA_MOCK").as_deref() == Ok("1");
        let client = if is_mock {
            DaemonClient::connect_mock(None).ok()
        } else {
            None
        };

        Self {
            is_mock,
            client: Mutex::new(client),
            socket_path,
            jobs: RwLock::new(HashMap::new()),
        }
    }

    /// Creates an in-memory mock server state for testing.
    pub fn new_mock() -> Self {
        Self::new(true)
    }
}

fn evict_excess_jobs(jobs: &mut HashMap<String, StoredScanJob>, max_jobs: usize) {
    while jobs.len() >= max_jobs {
        let victim = jobs
            .iter()
            .filter(|(_, job)| job.retrieved)
            .min_by_key(|(_, job)| job.created_at)
            .map(|(k, _)| k.clone())
            .or_else(|| {
                jobs.iter()
                    .min_by_key(|(_, job)| job.created_at)
                    .map(|(k, _)| k.clone())
            });
        match victim {
            Some(key) => {
                jobs.remove(&key);
            }
            None => break,
        }
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
    let target_dpi_u64 = u64::from(target_dpi);
    let (max_w, max_h) = resolution.max_flatbed_pixels();

    let x_offset = ((u64::from(req.x_offset.unwrap_or(0)) * target_dpi_u64) / 300)
        .min(u64::from(max_w.saturating_sub(1))) as u32;
    let y_offset = ((u64::from(req.y_offset.unwrap_or(0)) * target_dpi_u64) / 300)
        .min(u64::from(max_h.saturating_sub(1))) as u32;
    let width = ((u64::from(req.width.unwrap_or(2550)) * target_dpi_u64) / 300)
        .clamp(1, u64::from(max_w.saturating_sub(x_offset))) as u32;
    let height = ((u64::from(req.height.unwrap_or(3508)) * target_dpi_u64) / 300)
        .clamp(1, u64::from(max_h.saturating_sub(y_offset))) as u32;

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

    let raw_bytes = if state.is_mock {
        let mut guard = state.client.lock().await;
        if guard.is_none() {
            let mock_client = DaemonClient::connect_mock(None)
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            *guard = Some(mock_client);
        }
        guard
            .as_mut()
            .expect("mock client initialized")
            .acquire_scan(&params)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    } else {
        let mut temp_client =
            DaemonClient::connect_live(state.socket_path.as_deref(), None)
                .await
                .map_err(|e| {
                    (
                        StatusCode::SERVICE_UNAVAILABLE,
                        format!("Scanner hardware/daemon unavailable: {e}"),
                    )
                })?;
        temp_client
            .acquire_scan(&params)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    };

    let rgb_img = decode_raw_scan_buffer(raw_bytes, width, height, color_mode)
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("Decode error: {e}")))?;

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
            if processed.len() > 1 {
                tracing::warn!(
                    "eSCL PNG output truncated from {} images to 1; use application/pdf for multi-page",
                    processed.len()
                );
            }
            let bytes = export_png(&processed[0])
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
            ("image/png", bytes)
        }
        "image/jpeg" => {
            if processed.len() > 1 {
                tracing::warn!(
                    "eSCL JPEG output truncated from {} images to 1; use application/pdf for multi-page",
                    processed.len()
                );
            }
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
        evict_excess_jobs(&mut jobs_guard, MAX_RETAINED_JOBS);
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
        .layer(DefaultBodyLimit::max(MAX_ESCL_XML_BODY_BYTES))
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

    #[tokio::test]
    async fn test_escl_live_offline_returns_503_instead_of_mock() {
        let missing_sock = std::env::temp_dir().join(format!(
            "pixma_escl_missing_{}.sock",
            std::process::id()
        ));
        let state = Arc::new(EsclServerState::with_socket(false, Some(missing_sock)));
        let app = create_escl_router(state);

        let scan_settings = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03">
  <scan:ColorMode>RGB24</scan:ColorMode>
  <scan:XResolution>300</scan:XResolution>
  <scan:YResolution>300</scan:YResolution>
  <scan:DocumentFormat>application/pdf</scan:DocumentFormat>
</scan:ScanSettings>"#;

        let req = Request::builder()
            .uri("/eSCL/ScanJobs")
            .method("POST")
            .header("Content-Type", "text/xml")
            .body(Body::from(scan_settings))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn test_evict_excess_jobs_prefers_retrieved_and_caps_total() {
        let mut jobs = HashMap::new();
        for idx in 0..MAX_RETAINED_JOBS {
            let uuid = format!("job-{idx}");
            jobs.insert(
                uuid.clone(),
                StoredScanJob {
                    uuid,
                    content_type: "application/pdf".to_string(),
                    document_bytes: vec![1, 2, 3],
                    created_at: Instant::now(),
                    retrieved: idx == 10,
                },
            );
        }
        assert_eq!(jobs.len(), MAX_RETAINED_JOBS);
        evict_excess_jobs(&mut jobs, MAX_RETAINED_JOBS);
        assert_eq!(jobs.len(), MAX_RETAINED_JOBS - 1);
        assert!(
            !jobs.contains_key("job-10"),
            "Retrieved job should be evicted before unretrieved jobs"
        );
    }

    #[tokio::test]
    async fn test_escl_scan_region_u32_max_clamps_without_overflow() {
        let state = Arc::new(EsclServerState::new(true));
        let app = create_escl_router(state);

        let scan_settings = r#"<?xml version="1.0" encoding="UTF-8"?>
<scan:ScanSettings xmlns:scan="http://schemas.hp.com/imaging/escl/2011/05/03" xmlns:pwg="http://www.pwg.org/schemas/2010/12/sm">
  <scan:ColorMode>RGB24</scan:ColorMode>
  <scan:XResolution>600</scan:XResolution>
  <scan:YResolution>600</scan:YResolution>
  <scan:DocumentFormat>image/png</scan:DocumentFormat>
  <pwg:ScanRegions>
    <pwg:ScanRegion>
      <pwg:XOffset>4294967295</pwg:XOffset>
      <pwg:YOffset>4294967295</pwg:YOffset>
      <pwg:Width>4294967295</pwg:Width>
      <pwg:Height>4294967295</pwg:Height>
    </pwg:ScanRegion>
  </pwg:ScanRegions>
</scan:ScanSettings>"#;

        let req = Request::builder()
            .uri("/eSCL/ScanJobs")
            .method("POST")
            .header("Content-Type", "text/xml")
            .body(Body::from(scan_settings))
            .unwrap();

        let resp = app.oneshot(req).await.unwrap();
        assert_eq!(resp.status(), StatusCode::CREATED);
    }
}
