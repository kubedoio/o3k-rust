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
    resource::{
        CreateRequest, MutationResult, ResourceApplication, ResourceApplicationError,
        ResourceDescriptor,
    },
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
        // Establish non-disclosure from the authoritative durable resource
        // owner before touching canonical metadata.  A corrupt foreign row
        // must be indistinguishable from a missing operation.
        let durable = self.store.get_operation(id).await.map_err(|error| {
            if matches!(error, o3k_store::StoreError::OperationNotFound) {
                NativeReadError::NotFound
            } else {
                tracing::error!(%error, operation_id = %id, "native operation owner lookup failed");
                NativeReadError::Internal
            }
        })?;
        let resource_id = durable.resource_id;
        let resource = self.store.get_resource(resource_id).await.map_err(|error| {
            if matches!(error, o3k_store::StoreError::ResourceNotFound) {
                NativeReadError::NotFound
            } else {
                tracing::error!(%error, operation_id = %id, "native operation resource lookup failed");
                NativeReadError::Internal
            }
        })?;
        if resource.project_id != auth.effective_scope().id().as_str()
            || auth.effective_scope().kind() != o3k_kernel::ScopeKind::Project
        {
            return Err(NativeReadError::NotFound);
        }
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
        if operation.owner_scope.kind() != auth.effective_scope().kind()
            || operation.owner_scope.id() != auth.effective_scope().id()
        {
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
        CanonicalOperationRecord, DurableStore, IdempotencyReservationRequest, OperationRecord,
        OperationState, ResourceRecord,
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
                kind: "compute:server".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 1,
                desired_state: "active".into(),
                observed_state: "active".into(),
                provider_id: Some("secret-provider-resource".into()),
            })
            .await
            .expect("resource");
        let durable = OperationRecord {
            id,
            resource_id,
            kind: "native:create".into(),
            state: OperationState::Succeeded,
            provider_operation_id: Some("secret-provider-op".into()),
            error_category: None,
            error_message: Some("secret provider detail".into()),
        };
        let canonical = CanonicalOperationRecord {
            id,
            service: "compute".into(),
            action: "compute:CreateServer".into(),
            actor: "user-project-a".into(),
            owner_scope: "project-a".into(),
            resource_type: "compute:server".into(),
            resource_id: Some(resource_id.to_string()),
            state: OperationState::Succeeded,
            attempt: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: Some("2026-01-01T00:00:01Z".into()),
            error: None,
            request_id: Some("req-a".into()),
        };
        let reservation = IdempotencyReservationRequest::from_semantics(
            "project-a",
            "compute:CreateServer",
            "native-operation-visibility",
            "compute:server",
            Some(&resource_id.to_string()),
            &serde_json::json!({"resource_id": resource_id}),
            id,
        )
        .expect("idempotency identity");
        store
            .create_or_replay_canonical_idempotent_operation(&durable, &canonical, &reservation)
            .await
            .expect("canonical operation triplet");
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
        .expect("test manifest registry is valid")
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
        for forbidden in [
            "provider_operation_id",
            "provider_resource_id",
            "secret-provider-op",
            "secret-provider-resource",
            "secret provider detail",
            "agent_id",
            "agent_epoch",
            "database",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "native operation response leaked `{forbidden}`"
            );
        }

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

/// Composition-root application adapter for generic native reads. It delegates
/// only to canonical native application/read ports; it never reaches a
/// provider or controller directly. Mutations remain unsupported until a
/// canonical mutation service is wired for the resource.
pub struct GenericResourceApplication {
    pub compute: Arc<o3k_compute::ComputeService>,
    pub network_service: Arc<o3k_network::NetworkService>,
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub server: Arc<dyn o3k_native_api::compute::ServerReader>,
    pub volume: Arc<dyn o3k_native_api::volume::VolumeReader>,
    pub network: Arc<dyn o3k_native_api::network::NetworkReader>,
}

fn compute_error(error: o3k_compute::ComputeError) -> ResourceApplicationError {
    match error {
        o3k_compute::ComputeError::Unauthorized => ResourceApplicationError::Forbidden,
        o3k_compute::ComputeError::NotFound => ResourceApplicationError::NotFound,
        o3k_compute::ComputeError::InvalidRequest => ResourceApplicationError::Validation,
        o3k_compute::ComputeError::Conflict => ResourceApplicationError::Conflict,
        _ => ResourceApplicationError::Internal,
    }
}

fn generic_read_error(error: o3k_native_api::error::NativeReadError) -> ResourceApplicationError {
    match error {
        o3k_native_api::error::NativeReadError::NotFound => ResourceApplicationError::NotFound,
        o3k_native_api::error::NativeReadError::Forbidden => ResourceApplicationError::Forbidden,
        o3k_native_api::error::NativeReadError::Internal => ResourceApplicationError::Internal,
    }
}

fn server_json(item: ServerItem) -> serde_json::Value {
    serde_json::json!({"api_version":"o3k.io/v1","kind":"compute:server","metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation,"created_at":item.created_at},"spec":{"name":item.name,"flavor_id":item.flavor_id,"image_id":item.image_id},"status":{"state":item.state}})
}

fn volume_json(item: VolumeItem) -> serde_json::Value {
    serde_json::json!({"api_version":"o3k.io/v1","kind":"volume:volume","metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation,"created_at":item.created_at},"spec":{"size_bytes":item.size_bytes,"volume_type":item.volume_type},"status":{"state":item.state}})
}

fn realm_json(item: AddressRealmItem) -> serde_json::Value {
    serde_json::json!({"api_version":"o3k.io/v1","kind":"network:address_realm","metadata":{"id":item.id,"owner_scope":item.project_id,"generation":item.generation,"created_at":item.created_at},"spec":{"prefix":item.prefix,"overlapping_prefixes":item.overlapping_prefixes},"status":{"state":item.state}})
}

fn network_json(item: o3k_store::NetworkRecord) -> serde_json::Value {
    serde_json::json!({
        "api_version":"o3k.io/v1",
        "kind":"network:network",
        "metadata":{"id":item.id,"owner_scope":item.project_id,"generation":1},
        "spec":{"name":item.name},
        "status":{"state":item.status}
    })
}

#[async_trait::async_trait]
impl ResourceApplication for GenericResourceApplication {
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError> {
        match descriptor.resource_type.to_string().as_str() {
            "compute:server" => self
                .server
                .list_servers(auth)
                .await
                .map(|items| items.into_iter().map(server_json).collect())
                .map_err(generic_read_error),
            "network:address_realm" => self
                .network
                .list_address_realms(auth)
                .await
                .map(|items| items.into_iter().map(realm_json).collect())
                .map_err(generic_read_error),
            "network:network" => self
                .network_service
                .list_networks(auth)
                .await
                .map(|items| items.into_iter().map(network_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            "volume:volume" => self
                .volume
                .list_volumes(auth)
                .await
                .map(|items| items.into_iter().map(volume_json).collect())
                .map_err(generic_read_error),
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError> {
        let id = id
            .parse::<Uuid>()
            .map_err(|_| ResourceApplicationError::NotFound)?;
        match descriptor.resource_type.to_string().as_str() {
            "compute:server" => self
                .server
                .show_server(auth, id)
                .await
                .map(server_json)
                .map_err(generic_read_error),
            "network:address_realm" => self
                .network
                .show_address_realm(auth, id)
                .await
                .map(realm_json)
                .map_err(generic_read_error),
            "network:network" => self
                .network_service
                .get_network(auth, id)
                .await
                .map(network_json)
                .map_err(|_| ResourceApplicationError::NotFound),
            "volume:volume" => self
                .volume
                .show_volume(auth, id)
                .await
                .map(volume_json)
                .map_err(generic_read_error),
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        request: CreateRequest,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "network:network" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct NetworkSpec {
                name: String,
            }
            let spec: NetworkSpec = serde_json::from_value(request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?;
            let network = self
                .network_service
                .create_network(auth, spec.name)
                .await
                .map_err(|_| ResourceApplicationError::Conflict)?;
            return Ok(MutationResult {
                operation_id: format!("network:create:{}", network.id),
                resource_id: Some(network.id.to_string()),
                complete: true,
                resource: Some(network_json(network)),
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct ComputeSpec {
            name: String,
            image_id: String,
            flavor_id: Uuid,
            network_ids: Vec<String>,
            #[serde(default)]
            key_name: Option<String>,
        }
        let semantic_request = serde_json::json!({"spec": request.spec});
        let spec: ComputeSpec = serde_json::from_value(semantic_request["spec"].clone())
            .map_err(|_| ResourceApplicationError::Validation)?;
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Create)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let context = o3k_reconciler::CanonicalMutationContext::new(
            action,
            auth.principal().id().to_string(),
            auth.effective_scope().clone(),
            None,
            key.clone(),
            semantic_request,
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        let receipt = self
            .compute
            .create_server_for_auth_canonical(
                auth,
                o3k_compute::ServerCreateInput {
                    user_id: auth.principal().id().to_string(),
                    project_id: auth.effective_scope().id().as_str().to_owned(),
                    name: spec.name,
                    image_id: spec.image_id,
                    flavor_id: spec.flavor_id,
                    network_ids: spec.network_ids,
                    key_name: spec.key_name,
                    config_drive: None,
                    idempotency_key: key,
                },
                context,
            )
            .await
            .map_err(compute_error)?;
        let server = receipt.resource;
        let resource = self
            .store
            .get_resource(server.id.as_uuid())
            .await
            .map_err(|_| ResourceApplicationError::Internal)?;
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(server.id.as_uuid().to_string()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded
            ),
            resource: Some(server_json(ServerItem {
                id: server.id.as_uuid().to_string(),
                project_id: server.project_id,
                name: server.name,
                flavor_id: server.flavor_id.to_string(),
                image_id: server.image_id,
                state: format!("{:?}", server.state),
                generation: resource.generation,
                created_at: None,
            })),
        })
    }

    async fn delete(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError> {
        if descriptor.resource_type.to_string() == "network:network" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            self.network_service
                .delete_network(auth, resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            return Ok(MutationResult {
                operation_id: format!("network:delete:{id}"),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() != "compute:server" {
            return Err(ResourceApplicationError::UnsupportedOperation);
        }
        let key = idempotency_key
            .map(str::to_owned)
            .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
        let resource_id = id
            .parse::<Uuid>()
            .map_err(|_| ResourceApplicationError::NotFound)?;
        let existing = self
            .store
            .get_resource(resource_id)
            .await
            .map_err(|_| ResourceApplicationError::NotFound)?;
        if existing.project_id != auth.effective_scope().id().as_str() {
            return Err(ResourceApplicationError::NotFound);
        }
        let action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Delete)
            .cloned()
            .ok_or(ResourceApplicationError::UnsupportedOperation)?;
        let context = o3k_reconciler::CanonicalMutationContext::new(
            action,
            auth.principal().id().to_string(),
            auth.effective_scope().clone(),
            None,
            key,
            serde_json::json!({"resource_id": id}),
        )
        .map_err(|_| ResourceApplicationError::Validation)?;
        let receipt = self
            .compute
            .delete_server_for_auth_canonical(
                auth,
                o3k_domain::ServerId::from_uuid(resource_id),
                context,
            )
            .await
            .map_err(compute_error)?;
        Ok(MutationResult {
            operation_id: receipt.operation_id.to_string(),
            resource_id: Some(id.to_owned()),
            complete: matches!(
                receipt.operation_state,
                o3k_store::OperationState::Succeeded
            ),
            resource: None,
        })
    }
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

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod native_compute_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use o3k_kernel::{
        ActionId, AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, UserPrincipal,
    };
    use o3k_native_api::auth::{NativeTokenRequestV1, TokenIssuer};
    use o3k_provider::{FailureInjection, FakeComputeProvider};
    use std::sync::Arc;
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

    fn compute_manifest_registry() -> o3k_kernel::ManifestRegistry {
        use std::collections::HashMap;
        let mut reg = o3k_kernel::ManifestRegistry::new();
        let mut ops = HashMap::new();
        ops.insert(
            "create".to_owned(),
            ActionId::new_unchecked("compute", "CreateServer"),
        );
        ops.insert(
            "delete".to_owned(),
            ActionId::new_unchecked("compute", "DeleteServer"),
        );
        ops.insert(
            "list".to_owned(),
            ActionId::new_unchecked("compute", "ListServers"),
        );
        ops.insert(
            "show".to_owned(),
            ActionId::new_unchecked("compute", "ShowServer"),
        );
        let m = o3k_kernel::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "0.4.0".to_owned(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            resource_types: vec![o3k_kernel::RegisteredResourceType {
                resource_type: o3k_kernel::ResourceType::new_unchecked("compute", "server"),
                schema_version: "v1".to_owned(),
                collection: Some("servers".to_owned()),
                scope: o3k_kernel::ResourceScope::Tenant,
                operations: ops,
            }],
            actions: vec![
                "compute:ListServers".to_owned(),
                "compute:CreateServer".to_owned(),
                "compute:DeleteServer".to_owned(),
                "compute:ShowServer".to_owned(),
            ],
            capabilities: vec![],
            dependencies: vec![],
            quota_dimensions: vec![],
            regions: vec![],
            availability_domains: vec![],
            controller: Some(o3k_kernel::ManifestController {
                mode: "in-process".to_owned(),
                protocol: "in-process".to_owned(),
                protocol_version: "1.0".to_owned(),
                service_principal: None,
            }),
            health: None,
        };
        let _ = reg.register(m);
        let _ = reg.register_controller(
            "compute",
            o3k_kernel::controller::ControllerSession {
                service_id: "compute".to_owned(),
                namespace: "compute".to_owned(),
                service_principal: o3k_kernel::ServicePrincipal::new(
                    o3k_kernel::PrincipalId::new_unchecked("test-controller"),
                    "test-controller",
                    "compute",
                ),
                session_id: uuid::Uuid::new_v4(),
                session_generation: 1,
                protocol_version: o3k_kernel::controller::ProtocolVersion::new(1, 0),
                manifest_digest: "test-digest".to_owned(),
                manifest_generation: 1,
                started_at: "2026-01-01T00:00:00Z".to_owned(),
            },
        );
        let _ = reg.activate_controller("compute");
        reg
    }

    async fn setup() -> (
        axum::Router,
        Arc<o3k_store::unified::O3kStore>,
        Arc<FakeComputeProvider>,
    ) {
        use axum::Router;
        use axum::routing::get;
        use o3k_native_api::{operation, resource};

        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        let provider = Arc::new(FakeComputeProvider::new());
        let compute = Arc::new(o3k_compute::ComputeService::new(
            store.clone(),
            provider.clone(),
        ));
        let network_service = Arc::new(
            o3k_network::NetworkService::open(
                std::env::temp_dir().join(format!("o3k-native-test-{}", Uuid::new_v4())),
                store.clone(),
            )
            .await
            .expect("network service"),
        );

        let app = GenericResourceApplication {
            compute: compute.clone(),
            network_service,
            store: store.clone(),
            server: Arc::new(ServerReaderAdapter {
                service: compute.clone(),
            }),
            volume: Arc::new(VolumeReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::empty()),
            }),
            network: Arc::new(NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::empty()),
            }),
        };

        let native = o3k_native_api::NativeApiState::new(
            Some(compute_manifest_registry()),
            o3k_native_api::pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer)),
            Some(Arc::new(ServerReaderAdapter {
                service: compute.clone(),
            })),
            None,
            None,
        )
        .expect("native state")
        .with_operation_reader(Arc::new(OperationReaderAdapter {
            store: store.clone(),
        }))
        .with_resource_application(Arc::new(app))
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));

        // Build a minimal router that only has generic resource routes and
        // the operation route — we deliberately omit the concrete
        // /compute/servers GET-only route so that POST to the generic
        // {namespace}/{collection} route resolves correctly.
        let router = Router::new()
            .route(
                "/{namespace}/{collection}",
                get(resource::list).post(resource::create),
            )
            .route(
                "/{namespace}/{collection}/{id}",
                get(resource::show).delete(resource::delete),
            )
            .route("/operations/{id}", get(operation::show_operation))
            .with_state(native);

        (router, store, provider)
    }

    fn authed(path: &str, project: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer project-{project}"))
            .body(Body::empty())
            .expect("request")
    }

    fn authed_post(
        path: &str,
        project: &str,
        idempotency_key: &str,
        body: serde_json::Value,
    ) -> Request<Body> {
        Request::builder()
            .uri(path)
            .method("POST")
            .header("authorization", format!("Bearer project-{project}"))
            .header("content-type", "application/json")
            .header("idempotency-key", idempotency_key)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("request")
    }

    fn authed_delete(path: &str, project: &str, idempotency_key: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .method("DELETE")
            .header("authorization", format!("Bearer project-{project}"))
            .header("idempotency-key", idempotency_key)
            .body(Body::empty())
            .expect("request")
    }

    /// Helper: send a request through a cloned router and return (status, parsed JSON body).
    /// Panics if the body is not valid JSON.
    async fn exec(router: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.clone().oneshot(req).await.expect("request");
        let status = response.status();
        let body_bytes = &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(body_bytes).expect("json");
        (status, json)
    }

    // ── Tests ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn native_compute_create_and_read_operation() {
        let (router, _, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // POST create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(json["complete"].as_bool().unwrap());
        let operation_id = json["operation_id"].as_str().unwrap().to_owned();
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // GET /operations/{id}
        let (status, op) = exec(router, authed(&format!("/operations/{operation_id}"), "a")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(op["id"], operation_id);
        assert_eq!(op["action"]["namespace"], "compute");
        assert_eq!(op["action"]["action"], "CreateServer");
        assert_eq!(op["resource_type"]["namespace"], "compute");
        assert_eq!(op["resource_type"]["name"], "server");
        assert_eq!(op["resource_id"], resource_id);
        assert_eq!(op["owner_scope"]["id"], "project-a");
    }

    #[tokio::test]
    async fn native_compute_create_replay_equivalent() {
        let (router, _, provider) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // First create
        let (status, first) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(first["complete"].as_bool().unwrap());

        // Replay with same key
        let (status, replay) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(replay["operation_id"], first["operation_id"]);
        assert_eq!(replay["resource_id"], first["resource_id"]);
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_compute_create_changed_body_conflict() {
        let (router, _, provider) = setup().await;
        let router = &router;
        let body_a = serde_json::json!({
            "spec": {
                "name": "test-a",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });
        let body_b = serde_json::json!({
            "spec": {
                "name": "test-b",
                "image_id": "image-b",
                "flavor_id": "00000000-0000-0000-0000-000000000002",
                "network_ids": ["net-b"]
            }
        });

        // First create
        let (status, _) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body_a),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Replay with DIFFERENT body → 409 Conflict
        let (status, _) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body_b),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_compute_delete_returns_operation() {
        let (router, _, provider) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create first
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Set provider timeout so delete becomes async (202)
        provider
            .set_failure(FailureInjection::Timeout)
            .expect("set failure");

        // DELETE → 202 Accepted
        let (status, delete_json) = exec(
            router,
            authed_delete(&format!("/compute/servers/{resource_id}"), "a", "delete-A"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let operation_id = delete_json["operation_id"].as_str().unwrap().to_owned();
        assert!(!delete_json["complete"].as_bool().unwrap());

        // GET /operations/{id} shows the delete
        let (status, op) = exec(router, authed(&format!("/operations/{operation_id}"), "a")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(op["id"], operation_id);
        assert_eq!(op["action"]["namespace"], "compute");
        assert_eq!(op["action"]["action"], "DeleteServer");
        assert_eq!(op["resource_id"], resource_id);
        assert_eq!(op["owner_scope"]["id"], "project-a");

        // Replay delete with same idempotency key — same operation
        let (status, replay) = exec(
            router,
            authed_delete(&format!("/compute/servers/{resource_id}"), "a", "delete-A"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(replay["operation_id"], operation_id);
    }

    #[tokio::test]
    async fn native_compute_create_after_delete_same_key_fails() {
        let (router, _, provider) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Clear failure for synchronous delete
        provider
            .set_failure(FailureInjection::None)
            .expect("clear failure");

        // Delete (synchronous → 204 No Content)
        let response = router
            .clone()
            .oneshot(authed_delete(
                &format!("/compute/servers/{resource_id}"),
                "a",
                "delete-B",
            ))
            .await
            .expect("delete");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Create with SAME key — fail closed: the consumed idempotency key
        // cannot create a new resource, even after the original was deleted.
        let (status, _replay) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        // After deletion, the idempotency key is still bound to the original
        // create operation. Replaying returns the original resource (404 not
        // found is expected for a deleted resource — the system rejects the
        // request rather than silently creating a new resource with the same
        // key).
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn native_compute_create_after_delete_new_key_succeeds() {
        let (router, _, provider) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Clear failure for sync delete
        provider
            .set_failure(FailureInjection::None)
            .expect("clear failure");

        // Delete
        let response = router
            .clone()
            .oneshot(authed_delete(
                &format!("/compute/servers/{resource_id}"),
                "a",
                "delete-A",
            ))
            .await
            .expect("delete");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Create with NEW key — starts a new lifecycle
        let (status, recreate) = exec(
            router,
            authed_post("/compute/servers", "a", "create-B", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_ne!(recreate["resource_id"].as_str().unwrap(), resource_id);
        assert!(recreate["complete"].as_bool().unwrap());
    }
}
