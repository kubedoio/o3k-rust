//! Router-wide microversion negotiation layer shared by the Nova 2.1 and
//! Placement protocol adapters.

use axum::{
    Json,
    http::{HeaderValue, StatusCode},
    response::IntoResponse,
};

use crate::placement::parse_microversion;

/// Returns (parsed_version, malformed).
fn parse_service_version<'a>(
    headers: &'a axum::http::HeaderMap,
    service: &str,
) -> (Option<&'a str>, bool) {
    let os_api_ver = headers
        .get("OpenStack-API-Version")
        .and_then(|h| h.to_str().ok());

    let mut version: Option<&str> = None;
    let mut malformed = false;

    if let Some(val) = os_api_ver {
        for part in val.split(',') {
            let tokens: Vec<&str> = part.split_whitespace().collect();
            if tokens.len() == 2 && tokens[0].eq_ignore_ascii_case(service) {
                version = Some(tokens[1]);
                break;
            } else if tokens.len() != 2
                && !part.trim().is_empty()
                && part.trim().to_lowercase().contains(service)
            {
                malformed = true;
            }
        }
    }

    (version, malformed)
}

fn set_compute_vary(response: &mut axum::http::HeaderMap) {
    response.insert(
        "Vary",
        HeaderValue::from_static("OpenStack-API-Version, X-OpenStack-Nova-API-Version"),
    );
}

fn set_placement_vary(response: &mut axum::http::HeaderMap) {
    response.insert("Vary", HeaderValue::from_static("OpenStack-API-Version"));
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

        let (mut compute_version, malformed) = parse_service_version(req.headers(), "compute");

        let nova_api_ver = req
            .headers()
            .get("X-OpenStack-Nova-API-Version")
            .and_then(|h| h.to_str().ok());

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
            set_compute_vary(resp.headers_mut());
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
            set_compute_vary(resp.headers_mut());
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
        set_compute_vary(response.headers_mut());
        return response;
    }

    if path.starts_with("/placement") {
        if path == "/placement" || path == "/placement/" {
            return next.run(req).await;
        }

        let (placement_version, malformed) = parse_service_version(req.headers(), "placement");

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
            set_placement_vary(resp.headers_mut());
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
                set_placement_vary(resp.headers_mut());
                return resp;
            }
        }

        let mut response = next.run(req).await;
        if let Ok(header_val) = HeaderValue::from_str(&format!("placement {negotiated}")) {
            response
                .headers_mut()
                .insert("OpenStack-API-Version", header_val);
        }
        set_placement_vary(response.headers_mut());
        return response;
    }

    next.run(req).await
}
