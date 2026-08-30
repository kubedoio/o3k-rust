use std::sync::Arc;

use o3k_native_api::{compute::ServerItem, error::NativeReadError};
use uuid::Uuid;

/// Store-backed adapter for native Compute server reads.
pub struct ServerReaderAdapter {
    pub service: Arc<o3k_compute::ComputeService>,
}

/// Composition-root application adapter for generic native reads. It delegates
/// only to canonical native application/read ports; it never reaches a
/// provider or controller directly. Mutations remain unsupported until a
/// canonical mutation service is wired for the resource.
#[async_trait::async_trait]
impl o3k_native_api::compute::ServerReader for ServerReaderAdapter {
    async fn list_servers(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<ServerItem>, NativeReadError> {
        match self.service.list_servers_for_auth(auth).await {
            Ok(servers) => {
                let mut items = Vec::with_capacity(servers.len());
                for s in servers {
                    let id = s.id.as_uuid();
                    let generation = self.service.server_generation_for_auth(auth, s.id).await
                        .map_err(|error| {
                            tracing::error!(%error, server_id = %id, "native server metadata read failed");
                            NativeReadError::Internal
                        })?;
                    items.push(ServerItem {
                        id: id.to_string(),
                        name: s.name,
                        project_id: s.project_id,
                        flavor_id: s.flavor_id.to_string(),
                        image_id: s.image_id,
                        state: serde_json::to_value(s.state)
                            .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                            .unwrap_or_else(|_| "unknown".to_owned()),
                        created_at: None, // No durable timestamp available from domain Server
                        generation,
                    });
                }
                Ok(items)
            }
            Err(e) => {
                tracing::error!(error = %e, "native server list failed");
                Err(match e {
                    o3k_compute::ComputeError::Unauthorized => NativeReadError::Forbidden,
                    o3k_compute::ComputeError::NotFound => NativeReadError::NotFound,
                    _ => NativeReadError::Internal,
                })
            }
        }
    }

    async fn show_server(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<ServerItem, NativeReadError> {
        match self
            .service
            .show_server_for_auth(auth, o3k_domain::ServerId::from_uuid(id))
            .await
        {
            Ok(s) => {
                let generation = self.service.server_generation_for_auth(auth, s.id).await
                    .map_err(|error| {
                        tracing::error!(%error, server_id = %id, "native server metadata read failed");
                        NativeReadError::Internal
                    })?;
                Ok(ServerItem {
                    id: id.to_string(),
                    name: s.name,
                    project_id: s.project_id,
                    flavor_id: s.flavor_id.to_string(),
                    image_id: s.image_id,
                    state: serde_json::to_value(s.state)
                        .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                        .unwrap_or_else(|_| "unknown".to_owned()),
                    created_at: None,
                    generation,
                })
            }
            Err(e) => {
                tracing::error!(error = %e, server_id = %id, "native server show failed");
                Err(match e {
                    o3k_compute::ComputeError::Unauthorized
                    | o3k_compute::ComputeError::NotFound => NativeReadError::NotFound,
                    _ => NativeReadError::Internal,
                })
            }
        }
    }
}
