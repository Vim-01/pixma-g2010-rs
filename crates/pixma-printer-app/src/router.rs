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
use pixma_core::errors::DriverError;
use pixma_core::maintenance::MaintenanceAction;
use pixma_core::models::{DeviceStatus, OperationalState};
use pixma_core::print_params::PrintJobConfig;
use pixma_daemon::DaemonClient;
use pixma_raster::cups_reader::{parse_document_from_memory, RasterPageInput};
use pixma_raster::pipeline::PrintRasterPipeline;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use tokio::sync::Mutex;

const DEFAULT_PRINTER_URI_BASE: &str = "ipp://127.0.0.1:8631/ipp/print";
const MAX_IPP_BODY_BYTES: usize = 512 * 1024 * 1024;

/// Formats a normalized base IPP printer URI from the server bind socket address.
pub fn printer_uri_base_from_bind(addr: SocketAddr) -> String {
    let port = addr.port();
    match addr.ip() {
        IpAddr::V4(v4) => {
            let host = if v4.is_unspecified() {
                IpAddr::V4(std::net::Ipv4Addr::LOCALHOST)
            } else {
                IpAddr::V4(v4)
            };
            format!("ipp://{host}:{port}/ipp/print")
        }
        IpAddr::V6(v6) => {
            let host = if v6.is_unspecified() {
                std::net::Ipv6Addr::LOCALHOST
            } else {
                v6
            };
            format!("ipp://[{host}]:{port}/ipp/print")
        }
    }
}

fn is_valid_http_host(host: &str) -> bool {
    if host.is_empty()
        || !host.bytes().all(|b| (0x21..=0x7E).contains(&b))
        || host.contains('/')
        || host.contains('\\')
        || host.contains('@')
        || host.contains('?')
        || host.contains('#')
        || host.contains("://")
    {
        return false;
    }
    if let Some(after_open) = host.strip_prefix('[') {
        let Some((_addr, rest)) = after_open.split_once(']') else {
            return false;
        };
        if rest.is_empty() {
            return true;
        }
        let Some(port) = rest.strip_prefix(':') else {
            return false;
        };
        !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
    } else if let Some((name, port)) = host.split_once(':') {
        !name.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit())
    } else {
        true
    }
}

fn resolve_request_printer_uri(headers: &HeaderMap, fallback: &str) -> String {
    if let Some(raw_host) = headers.get(header::HOST).and_then(|h| h.to_str().ok()) {
        let trimmed = raw_host.trim();
        if is_valid_http_host(trimmed) {
            return format!("ipp://{trimmed}/ipp/print");
        }
    }
    fallback.to_string()
}

/// Shared application state holding the thread-safe printer client connection
/// and monotonically increasing job identifier counter.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<Mutex<Option<DaemonClient>>>,
    pub job_counter: Arc<AtomicU32>,
    pub printer_uri_base: String,
    pub socket_path: Option<PathBuf>,
}

impl AppState {
    pub fn new(client: DaemonClient) -> Self {
        Self::new_with_uri(client, DEFAULT_PRINTER_URI_BASE)
    }

    pub fn new_with_uri(client: DaemonClient, printer_uri_base: impl Into<String>) -> Self {
        Self {
            client: Arc::new(Mutex::new(Some(client))),
            job_counter: Arc::new(AtomicU32::new(1)),
            printer_uri_base: printer_uri_base.into(),
            socket_path: None,
        }
    }

    pub fn new_on_demand() -> Self {
        Self::new_on_demand_with_uri(DEFAULT_PRINTER_URI_BASE)
    }

    pub fn new_on_demand_with_uri(printer_uri_base: impl Into<String>) -> Self {
        Self::new_on_demand_with_socket_and_uri(None, printer_uri_base)
    }

    pub fn new_on_demand_with_socket_and_uri(
        socket_path: Option<PathBuf>,
        printer_uri_base: impl Into<String>,
    ) -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            job_counter: Arc::new(AtomicU32::new(1)),
            printer_uri_base: printer_uri_base.into(),
            socket_path,
        }
    }
}

fn offline_status(reason: &DriverError) -> DeviceStatus {
    DeviceStatus {
        state: OperationalState::Offline,
        status_detail: reason.to_string(),
        ..Default::default()
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
        .layer(DefaultBodyLimit::max(MAX_IPP_BODY_BYTES))
        .with_state(state)
}

/// Binary IPP/2.0 request dispatcher.
async fn handle_ipp_request(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, StatusCode> {
    let printer_uri_base = resolve_request_printer_uri(&headers, &state.printer_uri_base);
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
                client
                    .get_status()
                    .await
                    .unwrap_or_else(|e| offline_status(&e))
            } else {
                match DaemonClient::connect_live(state.socket_path.as_deref(), None).await {
                    Ok(mut temp_client) => temp_client
                        .get_status()
                        .await
                        .unwrap_or_else(|e| offline_status(&e)),
                    Err(e) => offline_status(&e),
                }
            };
            let resp = IppResponse::build_printer_attributes_with_uri(
                req.version_major,
                req.version_minor,
                req.request_id,
                &status,
                &printer_uri_base,
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
            let resp = IppResponse::build_create_job_response_with_uri(
                req.version_major,
                req.version_minor,
                req.request_id,
                job_id,
                &printer_uri_base,
            );
            resp.to_bytes()
        }

        OP_PRINT_JOB | OP_SEND_DOCUMENT => {
            if req.data.is_empty() {
                if req.operation_id == OP_SEND_DOCUMENT {
                    let job_id = state.job_counter.load(Ordering::SeqCst) as i32;
                    let resp = IppResponse::build_print_job_response_with_uri(
                        req.version_major,
                        req.version_minor,
                        req.request_id,
                        job_id,
                        &printer_uri_base,
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

            // Ingest PWG raster, multi-page PDF, or standard image at target hardware base DPI
            let target_dpi = config.quality.base_raster_dpi();
            let pages: Result<Vec<RasterPageInput>, _> =
                parse_document_from_memory(&req.data, Some(target_dpi));

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

            let timestamp = pixma_core::current_utc_timestamp_14();
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

            // Stream to device via bound client or fail-closed live USB/daemon connection
            let mut guard = state.client.lock().await;
            let print_res = if let Some(client) = guard.as_mut() {
                client.print_raw_stream(&job_stream).await
            } else {
                match DaemonClient::connect_live(state.socket_path.as_deref(), None).await {
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

            let resp = IppResponse::build_print_job_response_with_uri(
                req.version_major,
                req.version_minor,
                req.request_id,
                job_id,
                &printer_uri_base,
            );
            resp.to_bytes()
        }

        OP_GET_JOB_ATTRIBUTES => {
            let resp = IppResponse::build_print_job_response_with_uri(
                req.version_major,
                req.version_minor,
                req.request_id,
                1,
                &printer_uri_base,
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
        match DaemonClient::connect_live(state.socket_path.as_deref(), None).await {
            Ok(mut temp_client) => temp_client.get_status().await,
            Err(e) => Err(e),
        }
    };
    match res {
        Ok(status) => Ok(Json(status)),
        Err(e) => Err((
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: format!("Failed to query live printer status: {e}"),
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
        match DaemonClient::connect_live(state.socket_path.as_deref(), None).await {
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
        Err(e) => {
            let status_code = match &e {
                DriverError::Transport(_) | DriverError::Io(_) => StatusCode::SERVICE_UNAVAILABLE,
                _ => StatusCode::BAD_REQUEST,
            };
            Err((
                status_code,
                Json(ErrorResponse {
                    error: format!("Maintenance error: {e}"),
                }),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn printer_uri_base_from_bind_normalizes_wildcard_addresses() {
        assert_eq!(
            printer_uri_base_from_bind("0.0.0.0:8631".parse().unwrap()),
            "ipp://127.0.0.1:8631/ipp/print"
        );
        assert_eq!(
            printer_uri_base_from_bind("[::]:8631".parse().unwrap()),
            "ipp://[::1]:8631/ipp/print"
        );
        assert_eq!(
            printer_uri_base_from_bind("192.0.2.50:9631".parse().unwrap()),
            "ipp://192.0.2.50:9631/ipp/print"
        );
        assert_eq!(
            printer_uri_base_from_bind("[2001:db8::1]:8631".parse().unwrap()),
            "ipp://[2001:db8::1]:8631/ipp/print"
        );
    }

    #[test]
    fn resolve_request_printer_uri_validates_host_header_strictly() {
        let fallback = "ipp://127.0.0.1:8631/ipp/print";

        let mut ok_headers = HeaderMap::new();
        ok_headers.insert(header::HOST, HeaderValue::from_static("printer.local:8631"));
        assert_eq!(
            resolve_request_printer_uri(&ok_headers, fallback),
            "ipp://printer.local:8631/ipp/print"
        );

        let mut v6_headers = HeaderMap::new();
        v6_headers.insert(header::HOST, HeaderValue::from_static("[::1]:8631"));
        assert_eq!(
            resolve_request_printer_uri(&v6_headers, fallback),
            "ipp://[::1]:8631/ipp/print"
        );

        for bad in [
            "evil.com/path",
            "http://evil.com",
            "user@evil.com",
            "evil.com?x=1",
            "evil.com#frag",
            "bad host",
            "bad:notport",
            "[::1",
        ] {
            let mut h = HeaderMap::new();
            h.insert(header::HOST, HeaderValue::from_str(bad).unwrap());
            assert_eq!(
                resolve_request_printer_uri(&h, fallback),
                fallback,
                "malicious Host header '{bad}' must fall back"
            );
        }
    }
}
