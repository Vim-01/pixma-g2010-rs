//! Axum HTTP router exposing driverless IPP Everywhere binary endpoints
//! and JSON REST management APIs for the Canon PIXMA G2010.

use crate::ipp::{
    IppRequest, IppResponse, OP_GET_JOBS, OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB, OP_VALIDATE_JOB,
    STATUS_CLIENT_ERROR_BAD_REQUEST, STATUS_SERVER_ERROR_INTERNAL,
    STATUS_SERVER_ERROR_NOT_SUPPORTED,
};
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pixma_core::maintenance::MaintenanceAction;
use pixma_core::models::DeviceStatus;
use pixma_core::print_params::PrintJobConfig;
use pixma_daemon::DaemonClient;
use pixma_raster::cups_reader::{parse_image_from_memory, parse_pwg_raster, RasterPageInput};
use pixma_raster::pipeline::PrintRasterPipeline;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// Shared application state holding the thread-safe printer client connection
/// and monotonically increasing job identifier counter.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<Mutex<DaemonClient>>,
    pub job_counter: Arc<AtomicU32>,
}

impl AppState {
    pub fn new(client: DaemonClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
            job_counter: Arc::new(AtomicU32::new(1)),
        }
    }
}

/// JSON request body for maintenance routines.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MaintenanceApiRequest {
    pub action: MaintenanceAction,
    #[serde(default)]
    pub confirmation_token: Option<String>,
    #[serde(default)]
    pub override_cooldown: bool,
}

/// Standard JSON error response wrapper.
#[derive(Debug, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
}

/// Builds the complete Axum router with IPP Everywhere and REST API routes.
pub fn build_router(state: AppState) -> Router {
    Router::new()
        // Driverless IPP Everywhere endpoints
        .route("/ipp/print", post(handle_ipp_request))
        .route("/", post(handle_ipp_request))
        // REST management endpoints
        .route("/api/status", get(handle_get_status))
        .route("/api/maintenance", post(handle_post_maintenance))
        .with_state(state)
}

/// Binary IPP/2.0 request dispatcher.
async fn handle_ipp_request(
    State(state): State<AppState>,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let req = match IppRequest::parse(&body) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("Failed to parse IPP binary request: {e}");
            let resp = IppResponse::new(2, 0, STATUS_CLIENT_ERROR_BAD_REQUEST, 1);
            return Ok(ipp_binary_response(resp.to_bytes()));
        }
    };

    tracing::info!(
        "Received IPP request: op_id={:#06x}, req_id={}",
        req.operation_id,
        req.request_id
    );

    let resp_bytes = match req.operation_id {
        OP_GET_PRINTER_ATTRIBUTES => {
            let mut client = state.client.lock().await;
            let status = match client.get_status().await {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to query status from device: {e}");
                    DeviceStatus::default()
                }
            };
            let resp = IppResponse::build_printer_attributes(
                req.version_major,
                req.version_minor,
                req.request_id,
                &status,
            );
            resp.to_bytes()
        }

        OP_VALIDATE_JOB => {
            let resp = IppResponse::new(
                req.version_major,
                req.version_minor,
                crate::ipp::STATUS_SUCCESSFUL_OK,
                req.request_id,
            );
            resp.to_bytes()
        }

        OP_PRINT_JOB => {
            if req.data.is_empty() {
                let resp = IppResponse::new(
                    req.version_major,
                    req.version_minor,
                    STATUS_CLIENT_ERROR_BAD_REQUEST,
                    req.request_id,
                );
                return Ok(ipp_binary_response(resp.to_bytes()));
            }

            let job_id = state.job_counter.fetch_add(1, Ordering::SeqCst) as i32;

            // Ingest raster or image
            let pages: Result<Vec<RasterPageInput>, _> = if req.data.starts_with(b"RaS2") {
                parse_pwg_raster(&req.data)
            } else {
                parse_image_from_memory(&req.data, Some(300)).map(|p| vec![p])
            };

            let pages = match pages {
                Ok(p) if !p.is_empty() => p,
                Ok(_) => {
                    let resp = IppResponse::new(
                        req.version_major,
                        req.version_minor,
                        STATUS_CLIENT_ERROR_BAD_REQUEST,
                        req.request_id,
                    );
                    return Ok(ipp_binary_response(resp.to_bytes()));
                }
                Err(e) => {
                    tracing::error!("Failed to parse document payload: {e}");
                    let resp = IppResponse::new(
                        req.version_major,
                        req.version_minor,
                        STATUS_CLIENT_ERROR_BAD_REQUEST,
                        req.request_id,
                    );
                    return Ok(ipp_binary_response(resp.to_bytes()));
                }
            };

            let config = PrintJobConfig {
                job_id: job_id as u32,
                ..Default::default()
            };

            let timestamp = format_timestamp_now();
            let job_stream =
                match PrintRasterPipeline::compile_ivec_pwg_job(&pages, &config, &timestamp) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::error!("Failed to compile IVEC print stream: {e}");
                        let resp = IppResponse::new(
                            req.version_major,
                            req.version_minor,
                            STATUS_SERVER_ERROR_INTERNAL,
                            req.request_id,
                        );
                        return Ok(ipp_binary_response(resp.to_bytes()));
                    }
                };

            // Stream to device via client
            let mut client = state.client.lock().await;
            if let Err(e) = client.print_raw_stream(&job_stream).await {
                tracing::error!("Failed to stream print job to device: {e}");
                let resp = IppResponse::new(
                    req.version_major,
                    req.version_minor,
                    STATUS_SERVER_ERROR_INTERNAL,
                    req.request_id,
                );
                return Ok(ipp_binary_response(resp.to_bytes()));
            }

            let resp = IppResponse::build_print_job_response(
                req.version_major,
                req.version_minor,
                req.request_id,
                job_id,
            );
            resp.to_bytes()
        }

        OP_GET_JOBS => {
            let resp = IppResponse::new(
                req.version_major,
                req.version_minor,
                crate::ipp::STATUS_SUCCESSFUL_OK,
                req.request_id,
            );
            resp.to_bytes()
        }

        _ => {
            let resp = IppResponse::new(
                req.version_major,
                req.version_minor,
                STATUS_SERVER_ERROR_NOT_SUPPORTED,
                req.request_id,
            );
            resp.to_bytes()
        }
    };

    Ok(ipp_binary_response(resp_bytes))
}

/// Helper returning `application/ipp` HTTP response.
fn ipp_binary_response(body: Vec<u8>) -> Response {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/ipp"),
    );
    (StatusCode::OK, headers, body).into_response()
}

/// REST handler: `GET /api/status`.
async fn handle_get_status(
    State(state): State<AppState>,
) -> Result<Json<DeviceStatus>, (StatusCode, Json<ErrorResponse>)> {
    let mut client = state.client.lock().await;
    match client.get_status().await {
        Ok(status) => Ok(Json(status)),
        Err(e) => Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: format!("Failed to query status: {e}"),
            }),
        )),
    }
}

/// REST handler: `POST /api/maintenance`.
async fn handle_post_maintenance(
    State(state): State<AppState>,
    Json(payload): Json<MaintenanceApiRequest>,
) -> Result<Json<DeviceStatus>, (StatusCode, Json<ErrorResponse>)> {
    let mut client = state.client.lock().await;
    match client
        .execute_maintenance(
            payload.action,
            payload.confirmation_token.as_deref(),
            payload.override_cooldown,
        )
        .await
    {
        Ok(status) => Ok(Json(status)),
        Err(e) => Err((
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: format!("Maintenance error: {e}"),
            }),
        )),
    }
}

/// Formats current UTC timestamp as 14-character string `YYYYMMDDHHMMSS`.
fn format_timestamp_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let days = (secs / 86400) as i64;
    let rem_secs = (secs % 86400) as u32;
    let hour = rem_secs / 3600;
    let minute = (rem_secs % 3600) / 60;
    let second = rem_secs % 60;

    let z = days + 719468;
    let era = (if z >= 0 { z } else { z - 146096 }) / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1024 + doe / 1460 - doe / 36524) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}{m:02}{d:02}{hour:02}{minute:02}{second:02}")
}
