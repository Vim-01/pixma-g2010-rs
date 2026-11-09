//! Axum HTTP router exposing driverless IPP Everywhere binary endpoints
//! and JSON REST management APIs for the Canon PIXMA G2010.

use crate::ipp::{
    IppRequest, IppResponse, OP_CANCEL_JOB, OP_CREATE_JOB, OP_GET_JOBS, OP_GET_JOB_ATTRIBUTES,
    OP_GET_PRINTER_ATTRIBUTES, OP_PRINT_JOB, OP_SEND_DOCUMENT, OP_VALIDATE_JOB,
    STATUS_CLIENT_ERROR_BAD_REQUEST, STATUS_SERVER_ERROR_INTERNAL,
    STATUS_SERVER_ERROR_NOT_SUPPORTED,
};
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use pixma_core::maintenance::MaintenanceAction;
use pixma_core::models::DeviceStatus;
use pixma_core::print_params::PrintJobConfig;
use pixma_daemon::DaemonClient;
use pixma_raster::cups_reader::{parse_document_from_memory, RasterPageInput};
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
    pub client: Arc<Mutex<Option<DaemonClient>>>,
    pub job_counter: Arc<AtomicU32>,
}

impl AppState {
    pub fn new(client: DaemonClient) -> Self {
        Self {
            client: Arc::new(Mutex::new(Some(client))),
            job_counter: Arc::new(AtomicU32::new(1)),
        }
    }

    pub fn new_on_demand() -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
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
        .layer(DefaultBodyLimit::disable())
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
        "Received IPP request: op_id={:#06x}, req_id={}, payload_bytes={}",
        req.operation_id,
        req.request_id,
        req.data.len()
    );

    let resp_bytes = match req.operation_id {
        OP_GET_PRINTER_ATTRIBUTES => {
            let mut guard = state.client.lock().await;
            let status = if let Some(client) = guard.as_mut() {
                client.get_status().await.unwrap_or_default()
            } else {
                // In on-demand USB mode, return Idle capabilities immediately without
                // claiming USB so CUPS pre-job attribute polling is instant and lock-free.
                DeviceStatus::default()
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

        OP_CREATE_JOB => {
            let job_id = state.job_counter.fetch_add(1, Ordering::SeqCst) as i32;
            let resp = IppResponse::build_create_job_response(
                req.version_major,
                req.version_minor,
                req.request_id,
                job_id,
            );
            resp.to_bytes()
        }

        OP_PRINT_JOB | OP_SEND_DOCUMENT => {
            if req.data.is_empty() {
                if req.operation_id == OP_SEND_DOCUMENT {
                    let job_id = state.job_counter.load(Ordering::SeqCst) as i32;
                    let resp = IppResponse::build_print_job_response(
                        req.version_major,
                        req.version_minor,
                        req.request_id,
                        job_id,
                    );
                    return Ok(ipp_binary_response(resp.to_bytes()));
                }
                let resp = IppResponse::new(
                    req.version_major,
                    req.version_minor,
                    STATUS_CLIENT_ERROR_BAD_REQUEST,
                    req.request_id,
                );
                return Ok(ipp_binary_response(resp.to_bytes()));
            }

            let job_id = if req.operation_id == OP_PRINT_JOB {
                state.job_counter.fetch_add(1, Ordering::SeqCst) as i32
            } else {
                state.job_counter.load(Ordering::SeqCst).max(1) as i32
            };

            // Ingest PWG raster, multi-page PDF, or standard image at 300 DPI
            // (upscaled on-the-fly to 600x600 DPI hardware BJRaster3 during halftoning)
            let pages: Result<Vec<RasterPageInput>, _> =
                parse_document_from_memory(&req.data, Some(300));

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

            let mut config = PrintJobConfig {
                job_id: job_id as u32,
                ..Default::default()
            };
            // Honor IPP print-color-mode="monochrome" if requested
            if let Some(attr) = req
                .job_attributes
                .iter()
                .chain(req.operation_attributes.iter())
                .find(|a| a.name().eq_ignore_ascii_case("print-color-mode"))
            {
                if let Some(val) = attr.first_value().and_then(|v| v.as_str()) {
                    if val.contains("monochrome") || val.contains("gray") {
                        config.color.grayscale = true;
                    }
                }
            }

            let timestamp = format_timestamp_now();
            let job_stream = match PrintRasterPipeline::compile_ivec_bjraster3_job(
                &pages, &config, &timestamp,
            ) {
                Ok(s) => s,
                Err(e) => {
                    tracing::error!("Failed to compile IVEC BJRaster3 print stream: {e}");
                    let resp = IppResponse::new(
                        req.version_major,
                        req.version_minor,
                        STATUS_SERVER_ERROR_INTERNAL,
                        req.request_id,
                    );
                    return Ok(ipp_binary_response(resp.to_bytes()));
                }
            };

            // Stream to device via client (or on-demand USB/daemon connection)
            let mut guard = state.client.lock().await;
            let print_res = if let Some(client) = guard.as_mut() {
                client.print_raw_stream(&job_stream).await
            } else {
                match DaemonClient::connect_or_fallback(None, None).await {
                    Ok(mut temp_client) => temp_client.print_raw_stream(&job_stream).await,
                    Err(e) => Err(e),
                }
            };
            if let Err(e) = print_res {
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

        OP_GET_JOB_ATTRIBUTES => {
            let resp = IppResponse::build_print_job_response(
                req.version_major,
                req.version_minor,
                req.request_id,
                1,
            );
            resp.to_bytes()
        }

        OP_CANCEL_JOB | OP_GET_JOBS => {
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
    let mut guard = state.client.lock().await;
    let res = if let Some(client) = guard.as_mut() {
        client.get_status().await
    } else {
        match DaemonClient::connect_or_fallback(None, None).await {
            Ok(mut temp_client) => temp_client.get_status().await,
            Err(e) => Err(e),
        }
    };
    match res {
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
    let mut guard = state.client.lock().await;
    let res = if let Some(client) = guard.as_mut() {
        client
            .execute_maintenance(
                payload.action,
                payload.confirmation_token.as_deref(),
                payload.override_cooldown,
            )
            .await
    } else {
        match DaemonClient::connect_or_fallback(None, None).await {
            Ok(mut temp_client) => {
                temp_client
                    .execute_maintenance(
                        payload.action,
                        payload.confirmation_token.as_deref(),
                        payload.override_cooldown,
                    )
                    .await
            }
            Err(e) => Err(e),
        }
    };
    match res {
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
