//! Shared OpenStack error-envelope helpers used across the protocol
//! adapters.

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct KeystoneErrorResponse {
    error: KeystoneErrorBody,
}

#[derive(Serialize)]
pub(crate) struct KeystoneErrorBody {
    code: u16,
    title: &'static str,
    message: &'static str,
}

pub(crate) fn keystone_error(
    status: StatusCode,
    title: &'static str,
    message: &'static str,
) -> axum::response::Response {
    (
        status,
        Json(KeystoneErrorResponse {
            error: KeystoneErrorBody {
                code: status.as_u16(),
                title,
                message,
            },
        }),
    )
        .into_response()
}
