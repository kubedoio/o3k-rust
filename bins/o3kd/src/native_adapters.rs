//! Concrete adapter implementations for native API traits.
//!
//! Wired at the `o3kd` composition root where all service instances
//! are available. Internal errors are logged via tracing, NOT sent to
//! the client.

use std::sync::Arc;
use std::time::SystemTime;

use o3k_kernel::{
    ActionId, AuthorizationRequest, Authorizer, ResourceId, ResourceTarget, ResourceType,
};
use o3k_native_api::{
    auth::{NativeCredentialV1, NativeTokenRequestV1, TokenIssuer},
    compute::ServerItem,
    error::{NativeReadError, ProblemDetails},
    network::AddressRealmItem,
    volume::VolumeItem,
};
use o3k_store::{DurableStore, NetworkRepository, storage::StorageRepository};
use uuid::Uuid;

/// Store-backed canonical operation visibility adapter. Historical operation
/// rows without P12.4 metadata fail closed rather than being reconstructed
/// with fabricated ownership or action fields.
pub struct OperationReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::operation::OperationReader for OperationReaderAdapter {
    async fn show_operation(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<o3k_kernel::Operation, NativeReadError> {
        let record = self
            .store
            .get_canonical_operation(id)
            .await
            .map_err(|error| {
                if matches!(error, o3k_store::StoreError::OperationNotFound) {
                    NativeReadError::NotFound
                } else {
                    tracing::error!(%error, operation_id = %id, "native operation read failed");
                    NativeReadError::Internal
                }
            })?;
        let operation = o3k_kernel::Operation::try_from(record).map_err(|error| {
            tracing::error!(%error, operation_id = %id, "invalid canonical operation metadata");
            NativeReadError::Internal
        })?;
        if operation.owner_scope.id() != auth.effective_scope().id() {
            return Err(NativeReadError::NotFound);
        }
        Ok(operation)
    }
}

fn network_intent_state_wire(state: o3k_domain::NetworkIntentState) -> &'static str {
    match state {
        o3k_domain::NetworkIntentState::Requested => "requested",
        o3k_domain::NetworkIntentState::Active => "active",
        o3k_domain::NetworkIntentState::Deleting => "deleting",
        o3k_domain::NetworkIntentState::Error => "error",
    }
}

fn network_intent_identity_valid(
    record: &o3k_store::NetworkIntentRecord,
    intent: &o3k_domain::NetworkIntent,
) -> bool {
    record.id == intent.id
        && record.project_id == intent.project_id
        && intent.realm.project_id == record.project_id
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod operation_visibility_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use o3k_kernel::{AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, UserPrincipal};
    use o3k_native_api::auth::{NativeTokenRequestV1, TokenIssuer};
    use o3k_store::{
        CanonicalOperationRecord, DurableStore, OperationRecord, OperationState, ResourceRecord,
    };
    use std::path::PathBuf;
    use tower::util::ServiceExt;

    struct TestIssuer;

    fn context(project: &str) -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked(format!("user-{project}")),
                format!("user-{project}"),
                None,
            )),
            OwnershipScope::project(ScopeId::new_unchecked(project), None, None),
            vec!["member".into()],
            1,
            u64::MAX,
            "audit",
            "request",
            None,
        )
    }

    #[async_trait::async_trait]
    impl TokenIssuer for TestIssuer {
        async fn issue_native(
            &self,
            _request: &NativeTokenRequestV1,
        ) -> Result<(String, serde_json::Value), ProblemDetails> {
            Err(ProblemDetails::bad_request(
                "test issuer does not issue tokens",
            ))
        }

        async fn auth_context(&self, token: &str) -> Result<AuthContext, ProblemDetails> {
            token
                .strip_prefix("project-")
                .map(|project| context(&format!("project-{project}")))
                .ok_or_else(ProblemDetails::unauthorized)
        }
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("o3k-p12-4-op-api-{}.db", Uuid::new_v4()))
    }

    async fn seed() -> (Arc<o3k_store::unified::O3kStore>, Uuid, PathBuf) {
        let path = temp_db();
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_file(&path)
                .await
                .expect("sqlite store"),
        );
        let id = Uuid::new_v4();
        let resource_id = Uuid::new_v4();
        store
            .insert_resource(&ResourceRecord {
                id: resource_id,
                kind: "server".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 1,
                desired_state: "active".into(),
                observed_state: "active".into(),
                provider_id: Some("secret-provider-resource".into()),
            })
            .await
            .expect("resource");
        store
            .insert_operation(&OperationRecord {
                id,
                resource_id,
                kind: "native:create".into(),
                state: OperationState::Succeeded,
                provider_operation_id: Some("secret-provider-op".into()),
                error_category: None,
                error_message: Some("secret provider detail".into()),
            })
            .await
            .expect("operation");
        store
            .insert_canonical_operation(&CanonicalOperationRecord {
                id,
                service: "compute".into(),
                action: "compute:CreateServer".into(),
                actor: "user-project-a".into(),
                owner_scope: "project-a".into(),
                resource_type: "compute:server".into(),
                resource_id: Some(Uuid::new_v4().to_string()),
                state: OperationState::Succeeded,
                attempt: 1,
                created_at: "2026-01-01T00:00:00Z".into(),
                started_at: None,
                finished_at: Some("2026-01-01T00:00:01Z".into()),
                error: None,
                request_id: Some("req-a".into()),
            })
            .await
            .expect("canonical operation");
        // Exercise the same durable reconstruction path used after an o3kd
        // restart, rather than serving the record from the seeding pool.
        drop(store);
        let reopened = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_file(&path)
                .await
                .expect("reopen sqlite store"),
        );
        (reopened, id, path)
    }

    #[tokio::test]
    async fn operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields() {
        let (store, id, path) = seed().await;
        let reader = Arc::new(OperationReaderAdapter { store });
        let native = o3k_native_api::NativeApiState::new(
            None,
            o3k_native_api::pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer)),
            None,
            None,
            None,
        )
        .with_operation_reader(reader);
        let app = o3k_api::router_with_state(o3k_api::AppState::new().with_native_api(native));

        let request = |project: &str, operation: Uuid| {
            Request::builder()
                .uri(format!("/o3k/v1/operations/{operation}"))
                .header("authorization", format!("Bearer project-{project}"))
                .body(Body::empty())
                .expect("request")
        };
        let owner = app
            .clone()
            .oneshot(request("a", id))
            .await
            .expect("owner response");
        assert_eq!(owner.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(owner.into_body(), usize::MAX)
                .await
                .expect("body"),
        )
        .expect("json");
        assert_eq!(body["id"], id.to_string());
        assert_eq!(body["owner_scope"]["id"], "project-a");
        let serialized = body.to_string();
        assert!(!serialized.contains("provider_operation_id"));
        assert!(!serialized.contains("secret-provider-op"));
        assert!(!serialized.contains("secret provider detail"));

        let foreign = app
            .clone()
            .oneshot(request("b", id))
            .await
            .expect("foreign response");
        let missing = app
            .oneshot(request("b", Uuid::new_v4()))
            .await
            .expect("missing response");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let foreign_body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(foreign.into_body(), usize::MAX)
                .await
                .expect("foreign body"),
        )
        .expect("foreign json");
        let missing_body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(missing.into_body(), usize::MAX)
                .await
                .expect("missing body"),
        )
        .expect("missing json");
        assert_eq!(foreign_body["status"], missing_body["status"]);
        assert_eq!(foreign_body["code"], missing_body["code"]);
        assert_eq!(foreign_body["title"], missing_body["title"]);
        std::fs::remove_file(path).expect("cleanup");
    }
}

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

// ── VolumeReader ──────────────────────────────────────────────────────────

pub struct VolumeReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub authorizer: Arc<dyn Authorizer>,
}

fn authorize_collection(
    auth: &o3k_kernel::AuthContext,
    action: &str,
    namespace: &str,
    name: &str,
    authorizer: &dyn Authorizer,
) -> bool {
    let Ok(action) = ActionId::new(namespace, action.split(':').next_back().unwrap_or(action))
    else {
        return false;
    };
    let Ok(resource_type) = ResourceType::new(namespace, name) else {
        return false;
    };
    authorizer
        .authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::collection(
                resource_type,
                Some(auth.effective_scope().id().clone()),
            ),
        })
        .is_allowed()
}

fn authorize_instance(
    auth: &o3k_kernel::AuthContext,
    action: &str,
    namespace: &str,
    name: &str,
    id: Uuid,
    authorizer: &dyn Authorizer,
) -> bool {
    let Ok(action) = ActionId::new(namespace, action.split(':').next_back().unwrap_or(action))
    else {
        return false;
    };
    let Ok(resource_type) = ResourceType::new(namespace, name) else {
        return false;
    };
    let Ok(resource_id) = ResourceId::new(id.to_string()) else {
        return false;
    };
    authorizer
        .authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::instance(
                resource_type,
                resource_id,
                Some(auth.effective_scope().id().clone()),
            ),
        })
        .is_allowed()
}

#[async_trait::async_trait]
impl o3k_native_api::volume::VolumeReader for VolumeReaderAdapter {
    async fn list_volumes(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<VolumeItem>, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_collection(
            auth,
            "volume:ListVolumes",
            "volume",
            "volume",
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
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
                Err(NativeReadError::Internal)
            }
        }
    }

    async fn show_volume(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<VolumeItem, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_instance(
            auth,
            "volume:ReadVolume",
            "volume",
            "volume",
            id,
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
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
            Ok(_) => Err(NativeReadError::NotFound),
            Err(e) => {
                tracing::error!(error = %e, volume_id = %id, "native volume show failed");
                Err(NativeReadError::Internal)
            }
        }
    }
}

// ── NetworkReader ─────────────────────────────────────────────────────────

pub struct NetworkReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub authorizer: Arc<dyn Authorizer>,
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod network_reader_tests {
    use super::{authorize_collection, network_intent_state_wire};

    #[test]
    fn network_intent_state_is_serialized_from_canonical_state() {
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Requested),
            "requested"
        );
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Deleting),
            "deleting"
        );
        assert_eq!(
            network_intent_state_wire(o3k_domain::NetworkIntentState::Error),
            "error"
        );
    }

    #[test]
    fn denied_canonical_network_action_blocks_matching_scope() {
        let auth = o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("user-a"),
                "user-a",
                None,
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked("project-a"),
                None,
                None,
            ),
            vec!["member".into()],
            1,
            2,
            "audit",
            "request",
            None,
        );
        assert!(!authorize_collection(
            &auth,
            "network:ListAddressRealms",
            "network",
            "address_realm",
            &o3k_kernel::StaticAuthorizer::empty(),
        ));
    }
}

#[cfg(test)]
mod volume_reader_tests {
    use super::authorize_collection;

    #[test]
    fn denied_canonical_volume_action_blocks_matching_scope() {
        let auth = o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("user-b"),
                "user-b",
                None,
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked("project-b"),
                None,
                None,
            ),
            vec!["member".into()],
            1,
            2,
            "audit",
            "request",
            None,
        );
        assert!(!authorize_collection(
            &auth,
            "volume:ListVolumes",
            "volume",
            "volume",
            &o3k_kernel::StaticAuthorizer::empty(),
        ));
    }
}

#[async_trait::async_trait]
impl o3k_native_api::network::NetworkReader for NetworkReaderAdapter {
    async fn list_address_realms(
        &self,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<AddressRealmItem>, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_collection(
            auth,
            "network:ListAddressRealms",
            "network",
            "address_realm",
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        match self
            .store
            .list_network_intents(project_id)
            .await
        {
            Ok(records) => records
                .into_iter()
                .map(|record| {
                    let intent: o3k_domain::NetworkIntent =
                        serde_json::from_str(&record.payload).map_err(|e| {
                            tracing::error!(error = %e, network_intent_id = %record.id, "invalid canonical network intent payload");
                            NativeReadError::Internal
                        })?;
                    if !network_intent_identity_valid(&record, &intent) {
                        tracing::error!(network_intent_id = %record.id, "canonical network intent identity mismatch");
                        return Err(NativeReadError::Internal);
                    }
                    Ok(AddressRealmItem {
                        id: intent.realm.id.to_string(),
                        project_id: intent.realm.project_id,
                        prefix: format!("{}/{}", intent.realm.prefix.network, intent.realm.prefix.prefix_len),
                        overlapping_prefixes: intent.realm.overlapping_prefixes,
                        created_at: None,
                        generation: i64::try_from(intent.generation).map_err(|_| NativeReadError::Internal)?,
                        state: network_intent_state_wire(intent.state).to_owned(),
                    })
                })
                .collect(),
            Err(e) => {
                tracing::error!(error = %e, project_id = %project_id, "native address realm list failed");
                Err(NativeReadError::Internal)
            }
        }
    }

    async fn show_address_realm(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<AddressRealmItem, NativeReadError> {
        let project_id = auth.effective_scope().id().as_str();
        if !authorize_instance(
            auth,
            "network:ReadAddressRealm",
            "network",
            "address_realm",
            id,
            self.authorizer.as_ref(),
        ) {
            return Err(NativeReadError::Forbidden);
        }
        match self.store.list_network_intents(project_id).await {
            Ok(records) => {
                let Some(record) = records.into_iter().find(|record| {
                    serde_json::from_str::<o3k_domain::NetworkIntent>(&record.payload)
                        .map(|intent| intent.realm.id == id)
                        .unwrap_or(false)
                }) else {
                    return Err(NativeReadError::NotFound);
                };
                let intent: o3k_domain::NetworkIntent =
                    serde_json::from_str(&record.payload).map_err(|_| NativeReadError::Internal)?;
                if !network_intent_identity_valid(&record, &intent) || intent.realm.id != id {
                    return Err(NativeReadError::Internal);
                }
                Ok(AddressRealmItem {
                    id: intent.realm.id.to_string(),
                    project_id: intent.realm.project_id,
                    prefix: format!(
                        "{}/{}",
                        intent.realm.prefix.network, intent.realm.prefix.prefix_len
                    ),
                    overlapping_prefixes: intent.realm.overlapping_prefixes,
                    created_at: None,
                    generation: i64::try_from(intent.generation)
                        .map_err(|_| NativeReadError::Internal)?,
                    state: network_intent_state_wire(intent.state).to_owned(),
                })
            }
            Err(_) => Err(NativeReadError::Internal),
        }
    }
}
