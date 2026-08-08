//! Placement-compatible protocol adapter: version discovery and
//! microversion parsing.

use axum::{Json, response::IntoResponse};

pub(crate) async fn placement_discovery() -> impl IntoResponse {
    Json(serde_json::json!({
        "versions": [
            {
                "id": "v1.0",
                "status": "CURRENT",
                "min_version": "1.0",
                "max_version": "1.28",
                "links": [
                    {
                        "rel": "self",
                        "href": "/placement/"
                    }
                ]
            }
        ]
    }))
}

pub(crate) fn parse_microversion(ver: &str) -> Result<(u32, u32), ()> {
    let parts: Vec<&str> = ver.split('.').collect();
    if parts.len() == 2
        && let (Ok(major), Ok(minor)) = (parts[0].parse::<u32>(), parts[1].parse::<u32>())
    {
        return Ok((major, minor));
    }
    Err(())
}
