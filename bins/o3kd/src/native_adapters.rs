//! Concrete adapter implementations for the native API traits, wired at the
//! `o3kd` composition root where all service instances are available.

use std::sync::Arc;
use std::time::SystemTime;

use o3k_native_api::{
    auth::TokenIssuer,
    compute::{self as native_compute, ServerItem},
    error::{ErrorCode, ProblemDetails},
    volume::{self as native_volume, VolumeItem},
};
use uuid::Uuid;

// ── TokenIssuer ───────────────────────────────────────────────────────────

/// Adapter that wraps `TokenService` as a `TokenIssuer`.
pub struct TokenIssuerAdapter {
    pub service: Arc<o3k_identity::TokenService>,
}

#[async_trait::async_trait]
impl TokenIssuer for TokenIssuerAdapter {
    async fn issue(
        &self,
        request: &serde_json::Value,
        now: SystemTime,
    ) -> Result<(String, serde_json::Value), ProblemDetails> {
        let token_req: o3k_identity::TokenRequest = serde_json::from_value(request.clone())
            .map_err(|_| {
                ProblemDetails::with_detail(ErrorCode::BadRequest, "invalid auth request body")
            })?;
        match self.service.issue(&token_req, now) {
            Ok((token, response)) => {
                Ok((token, serde_json::to_value(response).unwrap_or_default()))
            }
            Err(e) => Err(ProblemDetails::with_detail(
                ErrorCode::Unauthorized,
                format!("authentication failed: {e}"),
            )),
        }
    }

    async fn auth_context(&self, token: &str) -> Result<o3k_kernel::AuthContext, ProblemDetails> {
        self.service
            .auth_context(token, SystemTime::now())
            .map_err(|_| ProblemDetails::unauthorized())
    }
}

// ── ServerReader ──────────────────────────────────────────────────────────

/// Adapter that wraps `ComputeService` as a `ServerReader`.
pub struct ServerReaderAdapter {
    pub service: Arc<o3k_compute::ComputeService>,
}

/// Converts a Uuid to an RFC3339 timestamp string by extracting the embedded
/// Unix ms timestamp from a UUIDv7 value. Returns a compact UTC ISO string
/// without pulling in chrono.
fn uuid_to_timestamp(id: Uuid) -> String {
    let unix_ms = id.as_u128() >> 80;
    if unix_ms == 0 {
        return "unknown".to_owned();
    }
    let secs = (unix_ms / 1000) as u64;
    let subsec_ms = (unix_ms % 1000) as u32;
    // Render as ISO 8601 / RFC 3339 without chrono.
    // Split secs into days and seconds within day.
    let days_since_epoch = secs / 86400;
    let secs_today = secs % 86400;
    let hours = secs_today / 3600;
    let mins = (secs_today % 3600) / 60;
    let seconds = secs_today % 60;

    // Days since epoch to calendar date using a simple algorithm valid
    // from 1970 to 2100.
    let mut y = 1970i64;
    let mut remaining = days_since_epoch as i64;
    loop {
        let days_in_year = if is_leap(y) { 366 } else { 365 };
        if remaining < days_in_year {
            break;
        }
        remaining -= days_in_year;
        y += 1;
    }
    let month_days = if is_leap(y) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };
    let mut m = 1u32;
    for &md in &month_days {
        if remaining < md {
            break;
        }
        remaining -= md;
        m += 1;
    }
    let d = remaining + 1;
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        y, m, d, hours, mins, seconds, subsec_ms
    )
}

fn is_leap(year: i64) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[async_trait::async_trait]
impl native_compute::ServerReader for ServerReaderAdapter {
    async fn list_servers(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<ServerItem>, ProblemDetails> {
        self.service
            .list_servers_for_auth(auth)
            .await
            .map(|servers| {
                servers
                    .into_iter()
                    .map(|s| {
                        let id = s.id.as_uuid();
                        ServerItem {
                            id: id.to_string(),
                            name: s.name,
                            project_id: s.project_id,
                            flavor_id: s.flavor_id.to_string(),
                            image_id: s.image_id,
                            state: serde_json::to_value(s.state)
                                .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                                .unwrap_or_else(|_| "unknown".to_owned()),
                            created_at: uuid_to_timestamp(id),
                        }
                    })
                    .collect()
            })
            .map_err(|e| ProblemDetails::with_detail(ErrorCode::InternalError, format!("{e}")))
    }

    async fn show_server(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<ServerItem, ProblemDetails> {
        self.service
            .show_server_for_auth(auth, o3k_domain::ServerId::from_uuid(id))
            .await
            .map(|s| ServerItem {
                id: id.to_string(),
                name: s.name,
                project_id: s.project_id,
                flavor_id: s.flavor_id.to_string(),
                image_id: s.image_id,
                state: serde_json::to_value(s.state)
                    .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                    .unwrap_or_else(|_| "unknown".to_owned()),
                created_at: uuid_to_timestamp(id),
            })
            .map_err(|e| match e {
                o3k_compute::ComputeError::NotFound => {
                    ProblemDetails::not_found(Some(&id.to_string()))
                }
                _ => ProblemDetails::with_detail(ErrorCode::InternalError, format!("{e}")),
            })
    }
}

// ── VolumeReader ──────────────────────────────────────────────────────────

/// Adapter that wraps a `StorageRepository` as a `VolumeReader`.
pub struct VolumeReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl native_volume::VolumeReader for VolumeReaderAdapter {
    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeItem>, ProblemDetails> {
        use o3k_store::storage::StorageRepository;
        self.store
            .list_volumes(project_id)
            .await
            .map(|records| {
                records
                    .into_iter()
                    .map(|r| VolumeItem {
                        id: r.volume.id.to_string(),
                        project_id: r.volume.project_id.clone(),
                        size_bytes: r.volume.size_bytes,
                        volume_type: r.volume.volume_type.clone(),
                        state: format!("{:?}", r.volume.state),
                        created_at: r.created_at.clone(),
                    })
                    .collect()
            })
            .map_err(|e| ProblemDetails::with_detail(ErrorCode::InternalError, format!("{e}")))
    }

    async fn show_volume(&self, project_id: &str, id: Uuid) -> Result<VolumeItem, ProblemDetails> {
        use o3k_store::storage::StorageRepository;
        self.store
            .get_volume(id)
            .await
            .map_err(|e| ProblemDetails::with_detail(ErrorCode::InternalError, format!("{e}")))?
            .filter(|r| r.volume.project_id == project_id)
            .map(|r| VolumeItem {
                id: r.volume.id.to_string(),
                project_id: r.volume.project_id.clone(),
                size_bytes: r.volume.size_bytes,
                volume_type: r.volume.volume_type.clone(),
                state: format!("{:?}", r.volume.state),
                created_at: r.created_at.clone(),
            })
            .ok_or_else(|| ProblemDetails::not_found(Some(&id.to_string())))
    }
}
