//! Router-wide microversion negotiation layer shared by the Nova 2.1 and
//! Placement protocol adapters.

use std::{
    fs::OpenOptions,
    io::Write,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

use axum::{
    Json,
    body::Body,
    http::{
        HeaderValue, StatusCode,
        header::{CONTENT_LENGTH, CONTENT_TYPE},
    },
    response::{IntoResponse, Response},
};
use sha2::{Digest, Sha256};
use tokio_stream::StreamExt;

use crate::placement::parse_microversion;

const TRACE_BODY_LIMIT: usize = 1024 * 1024;
static TRACE_ORDINAL: AtomicU64 = AtomicU64::new(0);
static TRACE_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn trace_path() -> Option<std::path::PathBuf> {
    std::env::var_os("O3K_COMPATIBILITY_TRACE_PATH").map(Into::into)
}

fn sensitive_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    [
        "authorization",
        "cookie",
        "token",
        "password",
        "secret",
        "private_key",
    ]
    .iter()
    .any(|part| name.contains(part))
}

fn redacted_json(value: serde_json::Value) -> serde_json::Value {
    redacted_json_inner(value, false)
}

fn redacted_json_inner(value: serde_json::Value, token_object: bool) -> serde_json::Value {
    match value {
        serde_json::Value::Object(entries) => serde_json::Value::Object(
            entries
                .into_iter()
                .map(|(key, value)| {
                    let value = if token_object && key == "id" {
                        serde_json::Value::String("<redacted>".to_owned())
                    } else if key == "token" && value.is_object() {
                        // Keystone uses `token` as the response envelope. Keep
                        // its catalog and scope visible, while nested token
                        // identifiers are redacted by the recursive pass.
                        redacted_json_inner(value, true)
                    } else if sensitive_name(&key) {
                        serde_json::Value::String("<redacted>".to_owned())
                    } else {
                        redacted_json_inner(value, false)
                    };
                    (key, value)
                })
                .collect(),
        ),
        serde_json::Value::Array(values) => serde_json::Value::Array(
            values
                .into_iter()
                .map(|value| redacted_json_inner(value, false))
                .collect(),
        ),
        value => value,
    }
}

fn body_record(bytes: &[u8], content_type: Option<&str>) -> serde_json::Value {
    let digest = format!("{:x}", Sha256::digest(bytes));
    if content_type.is_some_and(|value| value.to_ascii_lowercase().contains("json"))
        && let Ok(value) = serde_json::from_slice::<serde_json::Value>(bytes)
    {
        return serde_json::json!({
            "captured": true,
            "json": redacted_json(value),
            "sha256": digest,
            "content_length": bytes.len(),
        });
    }
    serde_json::json!({
        "captured": false,
        "sha256": digest,
        "content_length": bytes.len(),
        "content_type": content_type,
    })
}

fn headers_record(headers: &axum::http::HeaderMap) -> serde_json::Value {
    let values = headers.iter().map(|(name, value)| {
        let name = name.as_str().to_owned();
        let value = if sensitive_name(&name) {
            "<redacted>".to_owned()
        } else {
            value.to_str().unwrap_or("<invalid>").to_owned()
        };
        (name, serde_json::Value::String(value))
    });
    serde_json::Value::Object(values.collect())
}

fn bounded_content_length(headers: &axum::http::HeaderMap) -> Option<usize> {
    headers
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|length| *length <= TRACE_BODY_LIMIT)
}

fn trace_resource(path: &str) -> &'static str {
    if path == "/v3" || path == "/" || path == "/v2.1" || path == "/v2.1/" {
        "catalog"
    } else if path.starts_with("/v3/auth") {
        "auth"
    } else if path == "/v2.0/extensions" {
        "extensions"
    } else if path.starts_with("/placement") {
        "placement"
    } else if path.contains("/images") {
        "openstack_images_image_v2"
    } else if path.contains("/flavors") {
        "openstack_compute_flavor_v2"
    } else if path.contains("/os-keypairs") {
        "openstack_compute_keypair_v2"
    } else if path.contains("/networks") {
        "openstack_networking_network_v2"
    } else if path.contains("/subnets") {
        "openstack_networking_subnet_v2"
    } else if path.contains("/ports") {
        "openstack_networking_port_v2"
    } else if path.contains("/servers") {
        "openstack_compute_instance_v2"
    } else {
        "openstack-compatibility"
    }
}

fn append_trace(record: serde_json::Value) {
    let Some(path) = trace_path() else { return };
    let line = match serde_json::to_vec(&record) {
        Ok(line) => line,
        Err(_) => return,
    };
    let lock = TRACE_WRITE_LOCK.get_or_init(|| Mutex::new(()));
    let Ok(_guard) = lock.lock() else { return };
    let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) else {
        return;
    };
    let _ = file.write_all(&line);
    let _ = file.write_all(b"\n");
}

/// Captures exact compatibility-boundary metadata only when
/// `O3K_COMPATIBILITY_TRACE_PATH` is configured. It is transparent to
/// callers and never records unbounded or non-JSON payload contents.
pub(crate) async fn compatibility_trace_middleware(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = request.uri().path().to_owned();
    if trace_path().is_none() || !path.starts_with("/v") {
        return next.run(request).await;
    }
    let ordinal = TRACE_ORDINAL.fetch_add(1, Ordering::Relaxed);
    let (parts, body) = request.into_parts();
    let content_type = parts
        .headers
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let capture_request = bounded_content_length(&parts.headers).is_some()
        && content_type
            .as_deref()
            .is_some_and(|value| value.to_ascii_lowercase().contains("json"));
    let captured_request = Arc::new(Mutex::new(Vec::new()));
    let capture_sink = Arc::clone(&captured_request);
    let body = Body::from_stream(body.into_data_stream().map(move |chunk| {
        if capture_request
            && let Ok(bytes) = &chunk
            && let Ok(mut captured) = capture_sink.lock()
        {
            let remaining = TRACE_BODY_LIMIT.saturating_sub(captured.len());
            captured.extend_from_slice(&bytes[..bytes.len().min(remaining)]);
        }
        chunk
    }));
    let request = axum::extract::Request::from_parts(parts.clone(), body);
    let response = next.run(request).await;
    let request_body = if capture_request {
        captured_request
            .lock()
            .ok()
            .map(|bytes| body_record(&bytes, content_type.as_deref()))
    } else {
        Some(serde_json::json!({
            "captured": false,
            "content_length": parts.headers.get(CONTENT_LENGTH).and_then(|value| value.to_str().ok()),
            "reason": "non-json-or-unbounded",
        }))
    };
    finish_trace(ordinal, parts, path, content_type, request_body, response).await
}

async fn finish_trace(
    ordinal: u64,
    request_parts: axum::http::request::Parts,
    path: String,
    content_type: Option<String>,
    request_body: Option<serde_json::Value>,
    response: Response,
) -> Response {
    let status = response.status().as_u16();
    let response_headers = headers_record(response.headers());
    let response_content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let response_length = bounded_content_length(response.headers());
    let (response, response_body) = if response_content_type
        .as_deref()
        .is_some_and(|value| value.to_ascii_lowercase().contains("json"))
        && response_length.is_none_or(|length| length <= TRACE_BODY_LIMIT)
    {
        let (parts, body) = response.into_parts();
        match axum::body::to_bytes(body, TRACE_BODY_LIMIT).await {
            Ok(bytes) => (
                Response::from_parts(parts, Body::from(bytes.clone())),
                Some(body_record(&bytes, response_content_type.as_deref())),
            ),
            Err(_) => (Response::from_parts(parts, Body::empty()), None),
        }
    } else {
        (response, None)
    };
    append_trace(serde_json::json!({
        "ordinal": ordinal,
        "resource": trace_resource(&path),
        "method": request_parts.method.as_str(),
        "path": path,
        "query": request_parts.uri.query().unwrap_or_default(),
        "request_headers": headers_record(&request_parts.headers),
        "request_content_type": content_type,
        "request_body": request_body,
        "status": status,
        "response_headers": response_headers,
        "response_body": response_body,
    }));
    response
}

pub(crate) async fn microversion_middleware(
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    let path = req.uri().path().to_owned();

    if path.starts_with("/v2.1") {
        if path == "/v2.1" || path == "/v2.1/" {
            return next.run(req).await;
        }

        let headers = req.headers();
        let os_api_ver = headers
            .get("OpenStack-API-Version")
            .and_then(|h| h.to_str().ok());
        let nova_api_ver = headers
            .get("X-OpenStack-Nova-API-Version")
            .and_then(|h| h.to_str().ok());

        let mut compute_version: Option<&str> = None;
        let mut malformed = false;

        if let Some(val) = os_api_ver {
            for part in val.split(',') {
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("compute") {
                    compute_version = Some(tokens[1]);
                    break;
                } else if tokens.len() != 2
                    && !part.trim().is_empty()
                    && part.trim().to_lowercase().contains("compute")
                {
                    malformed = true;
                }
            }
        }

        if compute_version.is_none()
            && !malformed
            && let Some(val) = nova_api_ver
        {
            let trimmed = val.trim();
            if !trimmed.is_empty() {
                compute_version = Some(trimmed);
            }
        }

        if malformed {
            let body = serde_json::json!({
                "badRequest": {
                    "code": 400,
                    "message": "Invalid microversion header format."
                }
            });
            let mut resp = (StatusCode::BAD_REQUEST, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            resp.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
            resp.headers_mut().insert(
                "Vary",
                HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
            );
            return resp;
        }

        let is_attachment_route = path.contains("/os-volume_attachments");
        // The operation-scoped 2.89 profile is GET-only on the volume
        // attachment list/show operations that Cinder's attachment-delete
        // guard (bug #2004555) requires. Every other 2.89 request is rejected.
        let is_allowed_289 = is_attachment_route
            && req.method() == axum::http::Method::GET
            && compute_version == Some("2.89");

        if let Some(ver) = compute_version
            && ver != "2.1"
            && !is_allowed_289
        {
            let body = serde_json::json!({
                "computeFault": {
                    "code": 406,
                    "message": format!(
                        "Version {ver} is not supported by the API. Minimum supported version is 2.1 and maximum supported version is 2.1."
                    )
                }
            });
            let mut resp = (StatusCode::NOT_ACCEPTABLE, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            resp.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
            resp.headers_mut().insert(
                "Vary",
                HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
            );
            return resp;
        }

        let mut response = next.run(req).await;
        if is_allowed_289 {
            response.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.89"),
            );
            response.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.89"),
            );
        } else {
            response.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("compute 2.1"),
            );
            response.headers_mut().insert(
                "X-OpenStack-Nova-API-Version",
                HeaderValue::from_static("2.1"),
            );
        }
        response.headers_mut().insert(
            "Vary",
            HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
        );
        return response;
    }

    if path.starts_with("/placement") {
        if path == "/placement" || path == "/placement/" {
            return next.run(req).await;
        }

        let headers = req.headers();
        let os_api_ver = headers
            .get("OpenStack-API-Version")
            .and_then(|h| h.to_str().ok());

        let mut placement_version: Option<&str> = None;
        let mut malformed = false;

        if let Some(val) = os_api_ver {
            for part in val.split(',') {
                let tokens: Vec<&str> = part.split_whitespace().collect();
                if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case("placement") {
                    placement_version = Some(tokens[1]);
                    break;
                } else if tokens.len() != 2
                    && !part.trim().is_empty()
                    && part.trim().to_lowercase().contains("placement")
                {
                    malformed = true;
                }
            }
        }

        if malformed {
            let body = serde_json::json!({
                "error": {
                    "code": 400,
                    "message": "Invalid microversion header format."
                }
            });
            let mut resp = (StatusCode::BAD_REQUEST, Json(body)).into_response();
            resp.headers_mut().insert(
                "OpenStack-API-Version",
                HeaderValue::from_static("placement 1.0"),
            );
            resp.headers_mut()
                .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
            return resp;
        }

        let mut negotiated = "1.0".to_string();
        if let Some(ver) = placement_version {
            let is_valid = if ver.eq_ignore_ascii_case("latest") {
                negotiated = "1.28".to_string();
                true
            } else if let Ok((major, minor)) = parse_microversion(ver) {
                if major == 1 && minor <= 28 {
                    negotiated = ver.to_string();
                    true
                } else {
                    false
                }
            } else {
                false
            };

            if !is_valid {
                let body = serde_json::json!({
                    "error": {
                        "code": 406,
                        "message": format!(
                            "Version {ver} is not supported by Placement API. Minimum supported version is 1.0 and maximum supported version is 1.28."
                        )
                    }
                });
                let mut resp = (StatusCode::NOT_ACCEPTABLE, Json(body)).into_response();
                resp.headers_mut().insert(
                    "OpenStack-API-Version",
                    HeaderValue::from_static("placement 1.28"),
                );
                resp.headers_mut()
                    .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
                return resp;
            }
        }

        let mut response = next.run(req).await;
        if let Ok(header_val) = HeaderValue::from_str(&format!("placement {negotiated}")) {
            response
                .headers_mut()
                .insert("OpenStack-API-Version", header_val);
        }
        response
            .headers_mut()
            .insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
        return response;
    }

    next.run(req).await
}
