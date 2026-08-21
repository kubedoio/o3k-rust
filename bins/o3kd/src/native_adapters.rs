//! Concrete adapter implementations for native API traits.
//!
//! Wired at the `o3kd` composition root where all service instances
//! are available. Internal errors are logged via tracing, NOT sent to
//! the client.

use std::sync::Arc;
use std::time::SystemTime;

use o3k_native_api::{
    auth::{NativeCredentialV1, NativeTokenRequestV1, TokenIssuer},
    compute::ServerItem,
    error::ProblemDetails,
    network::AddressRealmItem,
    volume::VolumeItem,
};
use o3k_store::{ComputeRepository, DurableStore, storage::StorageRepository};
use uuid::Uuid;

// ── TokenIssuer ───────────────────────────────────────────────────────────

pub struct TokenIssuerAdapter {
    pub service: Arc<o3k_identity::TokenService>,
}

#[async_trait::async_trait]
impl TokenIssuer for TokenIssuerAdapter {
    async fn issue_native(
        &self,
        request: &NativeTokenRequestV1,
    ) -> Result<(String, serde_json::Value), ProblemDetails> {
        let credential = request
            .auth
            .credential()
            .map_err(ProblemDetails::bad_request)?;
        let (methods, password, token) = match credential {
            NativeCredentialV1::Password { user_id, password } => (
                vec!["password".to_owned()],
                Some(o3k_identity::PasswordIdentity {
                    user: o3k_identity::UserReference {
                        id: Some(user_id),
                        name: None,
                        domain: None,
                        password,
                    },
                }),
                None,
            ),
            NativeCredentialV1::Token { token } => (
                vec!["token".to_owned()],
                None,
                Some(o3k_identity::TokenIdentity { id: token }),
            ),
        };
        // Build a Keystone-compatible TokenRequest from native request
        let token_req = o3k_identity::TokenRequest {
            auth: o3k_identity::Auth {
                identity: o3k_identity::Identity {
                    methods,
                    password,
                    token,
                },
                scope: request
                    .auth
                    .project_id
                    .as_ref()
                    .map(|pid| o3k_identity::Scope {
                        project: Some(o3k_identity::ProjectReference {
                            id: Some(pid.clone()),
                            name: None,
                            domain: None,
                        }),
                    }),
            },
        };

        match self.service.issue(&token_req, SystemTime::now()) {
            Ok((token, response)) => match serde_json::to_value(response) {
                Ok(val) => Ok((token, val)),
                Err(_) => Err(ProblemDetails::internal()),
            },
            Err(_) => Err(ProblemDetails::unauthorized()),
        }
    }

    async fn auth_context(&self, token: &str) -> Result<o3k_kernel::AuthContext, ProblemDetails> {
        self.service
            .auth_context(token, SystemTime::now())
            .map_err(|_| ProblemDetails::unauthorized())
    }
}

// ── ServerReader ──────────────────────────────────────────────────────────

pub struct ServerReaderAdapter {
    pub service: Arc<o3k_compute::ComputeService>,
}

#[async_trait::async_trait]
impl o3k_native_api::compute::ServerReader for ServerReaderAdapter {
    async fn list_servers(&self, auth: &o3k_kernel::AuthContext) -> Result<Vec<ServerItem>, ()> {
        match self.service.list_servers_for_auth(auth).await {
            Ok(servers) => Ok(servers
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
                        created_at: None, // No durable timestamp available from domain Server
                        generation: 0,
                    }
                })
                .collect()),
            Err(e) => {
                tracing::error!(error = %e, "native server list failed");
                Err(())
            }
        }
    }

    async fn show_server(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<ServerItem, ()> {
        match self
            .service
            .show_server_for_auth(auth, o3k_domain::ServerId::from_uuid(id))
            .await
        {
            Ok(s) => Ok(ServerItem {
                id: id.to_string(),
                name: s.name,
                project_id: s.project_id,
                flavor_id: s.flavor_id.to_string(),
                image_id: s.image_id,
                state: serde_json::to_value(s.state)
                    .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                    .unwrap_or_else(|_| "unknown".to_owned()),
                created_at: None,
                generation: 0,
            }),
            Err(e) => {
                tracing::error!(error = %e, server_id = %id, "native server show failed");
                Err(())
            }
        }
    }
}

// ── VolumeReader ──────────────────────────────────────────────────────────

pub struct VolumeReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::volume::VolumeReader for VolumeReaderAdapter {
    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeItem>, ()> {
        match self.store.list_volumes(project_id).await {
            Ok(records) => Ok(records
                .into_iter()
                .map(|r| VolumeItem {
                    id: r.volume.id.to_string(),
                    project_id: r.volume.project_id.clone(),
                    size_bytes: r.volume.size_bytes,
                    volume_type: r.volume.volume_type.clone(),
                    state: serde_json::to_value(r.volume.state)
                        .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                        .unwrap_or_else(|_| "unknown".to_owned()),
                    created_at: Some(r.created_at.clone()),
                    generation: r.volume.generation as i64,
                })
                .collect()),
            Err(e) => {
                tracing::error!(error = %e, project_id = %project_id, "native volume list failed");
                Err(())
            }
        }
    }

    async fn show_volume(&self, project_id: &str, id: Uuid) -> Result<VolumeItem, ()> {
        match self.store.get_volume(id).await {
            Ok(Some(r)) if r.volume.project_id == project_id => Ok(VolumeItem {
                id: r.volume.id.to_string(),
                project_id: r.volume.project_id.clone(),
                size_bytes: r.volume.size_bytes,
                volume_type: r.volume.volume_type.clone(),
                state: serde_json::to_value(r.volume.state)
                    .map(|v| v.as_str().unwrap_or("unknown").to_owned())
                    .unwrap_or_else(|_| "unknown".to_owned()),
                created_at: Some(r.created_at.clone()),
                generation: r.volume.generation as i64,
            }),
            Ok(_) => Err(()),
            Err(e) => {
                tracing::error!(error = %e, volume_id = %id, "native volume show failed");
                Err(())
            }
        }
    }
}

// ── NetworkReader ─────────────────────────────────────────────────────────

pub struct NetworkReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::network::NetworkReader for NetworkReaderAdapter {
    async fn list_address_realms(&self, project_id: &str) -> Result<Vec<AddressRealmItem>, ()> {
        // Use generic resource records with kind "network:address_realm"
        match self
            .store
            .list_resources_by_kind("network:address_realm")
            .await
        {
            Ok(records) => {
                let filtered: Vec<AddressRealmItem> = records
                    .into_iter()
                    .filter(|r| r.project_id == project_id)
                    .map(|r| AddressRealmItem {
                        id: r.id.to_string(),
                        project_id: r.project_id,
                        prefix: r.desired_state.clone(),
                        overlapping_prefixes: false,
                        created_at: None,
                        generation: r.generation,
                    })
                    .collect();
                Ok(filtered)
            }
            Err(e) => {
                tracing::error!(error = %e, project_id = %project_id, "native address realm list failed");
                Ok(Vec::new()) // Empty list if no realms stored yet
            }
        }
    }

    async fn show_address_realm(&self, project_id: &str, id: Uuid) -> Result<AddressRealmItem, ()> {
        match self.store.get_resource(id).await {
            Ok(r) if r.kind == "network:address_realm" && r.project_id == project_id => {
                Ok(AddressRealmItem {
                    id: r.id.to_string(),
                    project_id: r.project_id,
                    prefix: r.desired_state.clone(),
                    overlapping_prefixes: false,
                    created_at: None,
                    generation: r.generation,
                })
            }
            _ => Err(()),
        }
    }
}
