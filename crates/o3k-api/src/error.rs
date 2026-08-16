use std::borrow::Cow;

use axum::{Json, http::StatusCode, response::IntoResponse};
use serde::Serialize;

#[derive(Serialize)]
pub(crate) struct KeystoneErrorResponse {
    error: KeystoneErrorBody,
}

#[derive(Serialize)]
pub(crate) struct KeystoneErrorBody {
    code: u16,
    title: Cow<'static, str>,
    message: Cow<'static, str>,
}

pub(crate) fn keystone_error(
    status: StatusCode,
    title: impl Into<Cow<'static, str>>,
    message: impl Into<Cow<'static, str>>,
) -> axum::response::Response {
    (
        status,
        Json(KeystoneErrorResponse {
            error: KeystoneErrorBody {
                code: status.as_u16(),
                title: title.into(),
                message: message.into(),
            },
        }),
    )
        .into_response()
}
