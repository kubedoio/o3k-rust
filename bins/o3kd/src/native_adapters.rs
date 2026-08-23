//! Concrete adapter implementations for native API traits.
//!
//! Wired at the `o3kd` composition root where all service instances
//! are available. Internal errors are logged via tracing, NOT sent to
//! the client.

use std::time::SystemTime;
use std::{collections::BTreeMap, sync::Arc};

use o3k_kernel::{
    ActionId, AuthorizationRequest, Authorizer, Controller, ResourceId, ResourceTarget,
    ResourceType,
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
    pub network: Arc<dyn o3k_native_api::network::NetworkReader>,
    pub external_controllers: Arc<BTreeMap<String, Arc<o3k_service_sdk::GrpcControllerAdapter>>>,
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

fn generic_volume_json(resource: &o3k_store::ResourceRecord) -> serde_json::Value {
    let spec = serde_json::from_str::<serde_json::Value>(&resource.desired_state)
        .unwrap_or_else(|_| serde_json::json!({}));
    serde_json::json!({
        "api_version":"o3k.io/v1",
        "kind":"volume:volume",
        "metadata":{"id":resource.id,"owner_scope":resource.project_id,"generation":resource.generation},
        "spec":spec,
        "status":{"state":resource.observed_state}
    })
}

fn generic_external_json(resource: &o3k_store::ResourceRecord) -> serde_json::Value {
    let spec = serde_json::from_str(&resource.desired_state).unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "api_version": "o3k.io/v1",
        "kind": resource.kind,
        "metadata": {
            "id": resource.id,
            "owner_scope": resource.project_id,
            "generation": resource.generation
        },
        "spec": spec,
        "status": {"state": resource.observed_state}
    })
}

#[async_trait::async_trait]
impl ResourceApplication for GenericResourceApplication {
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError> {
        if self
            .external_controllers
            .contains_key(&descriptor.owning_service)
        {
            return self
                .store
                .list_resources(
                    auth.effective_scope().id().as_str(),
                    &descriptor.resource_type.to_string(),
                )
                .await
                .map(|resources| resources.iter().map(generic_external_json).collect())
                .map_err(|_| ResourceApplicationError::Internal);
        }
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
                .store
                .list_resources(auth.effective_scope().id().as_str(), "volume")
                .await
                .map(|items| items.iter().map(generic_volume_json).collect())
                .map_err(|_| ResourceApplicationError::Internal),
            _ => Err(ResourceApplicationError::NotFound),
        }
    }

    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &o3k_kernel::AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError> {
        if self
            .external_controllers
            .contains_key(&descriptor.owning_service)
        {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != descriptor.resource_type.to_string()
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            return Ok(generic_external_json(&resource));
        }
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
                .store
                .get_resource(id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)
                .and_then(|resource| {
                    if resource.kind == "volume"
                        && resource.project_id == auth.effective_scope().id().as_str()
                    {
                        Ok(generic_volume_json(&resource))
                    } else {
                        Err(ResourceApplicationError::NotFound)
                    }
                }),
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
        if let Some(controller) = self.external_controllers.get(&descriptor.owning_service) {
            if !controller.health().await.healthy {
                return Err(ResourceApplicationError::NotReady);
            }
            // The descriptor is derived at startup and cannot reflect a later
            // controller outage.  Re-check readiness at the mutation boundary
            // so a Ready -> NotReady transition cannot accept new work.
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Create)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_identity = format!(
                "{}:{}:{}:{}",
                auth.effective_scope().id(),
                descriptor.resource_type,
                action,
                key
            );
            let resource_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, resource_identity.as_bytes());
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:create:{resource_id}", descriptor.resource_type).as_bytes(),
            );
            let desired_state = serde_json::to_string(&request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?;
            let resource = o3k_store::ResourceRecord {
                id: resource_id,
                kind: descriptor.resource_type.to_string(),
                project_id: auth.effective_scope().id().as_str().to_owned(),
                generation: 1,
                observed_generation: 0,
                desired_state,
                observed_state: "PROVISIONING".to_owned(),
                provider_id: None,
            };
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:create".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    descriptor.owning_service.clone(),
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    descriptor.resource_type.clone(),
                    Some(o3k_kernel::ResourceId::new_unchecked(
                        resource_id.to_string(),
                    )),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                &descriptor.resource_type.to_string(),
                Some(&resource_id.to_string()),
                &request.spec,
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            let acceptance = self
                .store
                .create_or_replay_canonical_resource_operation(
                    &resource, &operation, &canonical, &identity, None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            let (operation_id, resource_id, replayed) = match acceptance {
                o3k_store::CanonicalAcceptanceOutcome::Created {
                    operation_id,
                    resource_id,
                } => (operation_id, resource_id, false),
                o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id,
                    resource_id,
                } => (operation_id, resource_id, true),
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
            };
            if replayed {
                let existing = self
                    .store
                    .get_resource(resource_id)
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
                // An equivalent replay must not redrive an external mutation
                // while its canonical operation is still converging.  The
                // durable reconciler owns retry/recovery; this API call only
                // returns the existing canonical result.
                return Ok(MutationResult {
                    operation_id: operation_id.to_string(),
                    resource_id: Some(resource_id.to_string()),
                    complete: existing.observed_state == "READY",
                    resource: Some(generic_external_json(&existing)),
                });
            }
            let session = controller.session();
            let context = o3k_kernel::OperationContext {
                request_id: auth
                    .request_id()
                    .parse()
                    .map_err(|_| ResourceApplicationError::Internal)?,
                operation_id,
                action,
                service_id: descriptor.owning_service.clone(),
                owner_scope: auth.effective_scope().clone(),
                session_id: session.session_id,
                session_generation: session.session_generation,
                deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
                replay_identity: format!("parent:{operation_id}"),
                audit_correlation: format!("parent:{operation_id}"),
            };
            let parent_reference = o3k_kernel::ResourceReference {
                resource_type: descriptor.resource_type.clone(),
                resource_id: o3k_kernel::ResourceId::new_unchecked(resource_id.to_string()),
                generation: 1,
            };
            let delegation = controller
                .issue_parent_delegation(
                    &context,
                    auth.principal().id().to_string(),
                    &parent_reference,
                )
                .map_err(|_| ResourceApplicationError::Unauthorized)?;
            let outcome = controller
                .reconcile(o3k_kernel::ReconcileRequest {
                    context,
                    resource: o3k_kernel::ResourceSnapshot {
                        reference: parent_reference,
                        desired_spec: request.spec,
                        known_status: None,
                        owner_scope: auth.effective_scope().clone(),
                    },
                    delegation: Some(delegation),
                })
                .await;
            let complete = matches!(outcome, o3k_kernel::ReconcileOutcome::Succeeded { .. });
            let observed_state = match &outcome {
                o3k_kernel::ReconcileOutcome::Succeeded { .. } => "READY",
                o3k_kernel::ReconcileOutcome::Unknown { .. } => "UNKNOWN",
                o3k_kernel::ReconcileOutcome::Failed { .. }
                | o3k_kernel::ReconcileOutcome::Retryable { .. } => "ERROR",
                o3k_kernel::ReconcileOutcome::Accepted { .. } => "PROVISIONING",
            };
            let lifecycle_state = match &outcome {
                o3k_kernel::ReconcileOutcome::Succeeded { .. } => {
                    o3k_kernel::OperationState::Succeeded
                }
                o3k_kernel::ReconcileOutcome::Unknown { .. } => {
                    o3k_kernel::OperationState::UnknownOutcome
                }
                o3k_kernel::ReconcileOutcome::Retryable { .. } => {
                    o3k_kernel::OperationState::Retryable
                }
                o3k_kernel::ReconcileOutcome::Failed { .. } => o3k_kernel::OperationState::Failed,
                o3k_kernel::ReconcileOutcome::Accepted { .. } => {
                    o3k_kernel::OperationState::Running
                }
            };
            let now = chrono::Utc::now().to_rfc3339();
            let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
                lifecycle_state,
                1,
                Some(now.clone()),
                matches!(
                    lifecycle_state,
                    o3k_kernel::OperationState::Succeeded | o3k_kernel::OperationState::Failed
                )
                .then_some(now),
                None,
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_canonical_operation_lifecycle(operation_id, &lifecycle)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            self.store
                .update_resource(
                    resource_id,
                    1,
                    &resource.desired_state,
                    observed_state,
                    if complete { 1 } else { 0 },
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(resource_id.to_string()),
                complete,
                resource: Some(serde_json::json!({
                    "api_version": "o3k.io/v1",
                    "kind": descriptor.resource_type.to_string(),
                    "metadata": {"id": resource_id, "generation": 1},
                    "spec": resource.desired_state,
                    "status": {"state": if complete {"READY"} else {"PROVISIONING"}}
                })),
            });
        }
        if descriptor.resource_type.to_string() == "volume:volume" {
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct VolumeSpec {
                size_bytes: u64,
                volume_type: String,
            }
            let spec: VolumeSpec = serde_json::from_value(request.spec.clone())
                .map_err(|_| ResourceApplicationError::Validation)?;
            if spec.size_bytes == 0 || spec.volume_type.trim().is_empty() {
                return Err(ResourceApplicationError::Validation);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Create)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:{}", Uuid::new_v4()));
            let resource_id = Uuid::new_v5(&Uuid::NAMESPACE_OID, key.as_bytes());
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("volume:create:{resource_id}").as_bytes(),
            );
            let desired_state = serde_json::to_string(&request.spec)
                .map_err(|_| ResourceApplicationError::Validation)?;
            let resource = o3k_store::ResourceRecord {
                id: resource_id,
                kind: "volume".to_owned(),
                project_id: auth.effective_scope().id().as_str().to_owned(),
                generation: 1,
                observed_generation: 1,
                desired_state,
                observed_state: "AVAILABLE".to_owned(),
                provider_id: None,
            };
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:create".to_owned(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    "volume",
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    o3k_kernel::ResourceType::new_unchecked("volume", "volume"),
                    Some(o3k_kernel::ResourceId::new_unchecked(
                        resource_id.to_string(),
                    )),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let request_identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                "volume:volume",
                Some(&resource_id.to_string()),
                &request.spec,
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            let outcome = self
                .store
                .create_or_replay_canonical_resource_operation(
                    &resource,
                    &operation,
                    &canonical,
                    &request_identity,
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            let (operation_id, resource_id) = match outcome {
                o3k_store::CanonicalAcceptanceOutcome::Created {
                    operation_id,
                    resource_id,
                }
                | o3k_store::CanonicalAcceptanceOutcome::ExistingEquivalent {
                    operation_id,
                    resource_id,
                } => (operation_id, resource_id),
                o3k_store::CanonicalAcceptanceOutcome::Conflict => {
                    return Err(ResourceApplicationError::IdempotencyConflict);
                }
            };
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(resource_id.to_string()),
                complete: true,
                resource: Some(generic_volume_json(&resource)),
            });
        }
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
                operation_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("network:create:{}", network.id).as_bytes(),
                )
                .to_string(),
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
                    // Keep provider command identity scoped even when the
                    // client reuses the same canonical key in another tenant.
                    idempotency_key: format!("{}:{key}", auth.effective_scope().id()),
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
        if let Some(controller) = self.external_controllers.get(&descriptor.owning_service) {
            if !controller.health().await.healthy {
                return Err(ResourceApplicationError::NotReady);
            }
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != descriptor.resource_type.to_string()
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("{}:delete:{id}:{key}", descriptor.resource_type).as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:delete".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    descriptor.owning_service.clone(),
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    descriptor.resource_type.clone(),
                    Some(o3k_kernel::ResourceId::new_unchecked(id)),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                &descriptor.resource_type.to_string(),
                Some(id),
                &serde_json::json!({"resource_id": id}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            if self
                .store
                .create_or_replay_canonical_lifecycle_operation(&operation, &canonical, &identity)
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                == o3k_store::CanonicalAcceptanceOutcome::Conflict
            {
                return Err(ResourceApplicationError::IdempotencyConflict);
            }
            let session = controller.session();
            let context = o3k_kernel::OperationContext {
                request_id: auth
                    .request_id()
                    .parse()
                    .map_err(|_| ResourceApplicationError::Internal)?,
                operation_id,
                action,
                service_id: descriptor.owning_service.clone(),
                owner_scope: auth.effective_scope().clone(),
                session_id: session.session_id,
                session_generation: session.session_generation,
                deadline_unix_ms: chrono::Utc::now().timestamp_millis() as u64 + 60_000,
                replay_identity: format!("delete:{operation_id}"),
                audit_correlation: format!("delete:{operation_id}"),
            };
            let parent_reference = o3k_kernel::ResourceReference {
                resource_type: descriptor.resource_type.clone(),
                resource_id: o3k_kernel::ResourceId::new_unchecked(id),
                generation: resource.generation,
            };
            let delegation = controller
                .issue_parent_delegation(
                    &context,
                    auth.principal().id().to_string(),
                    &parent_reference,
                )
                .map_err(|_| ResourceApplicationError::Unauthorized)?;
            let outcome = controller
                .delete(o3k_kernel::DeleteRequest {
                    context,
                    resource: parent_reference,
                    owner_scope: auth.effective_scope().clone(),
                    delegation: Some(delegation),
                })
                .await;
            let complete = matches!(outcome, o3k_kernel::ReconcileOutcome::Succeeded { .. });
            if complete {
                self.store
                    .update_resource(
                        resource_id,
                        resource.generation,
                        "DELETED",
                        "DELETED",
                        resource.generation,
                        None,
                    )
                    .await
                    .map_err(|_| ResourceApplicationError::Internal)?;
            }
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() == "volume:volume" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            let resource = self
                .store
                .get_resource(resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            if resource.kind != "volume"
                || resource.project_id != auth.effective_scope().id().as_str()
            {
                return Err(ResourceApplicationError::NotFound);
            }
            let action = descriptor
                .lifecycle_actions
                .get(&o3k_native_api::resource::LifecycleOperation::Delete)
                .cloned()
                .ok_or(ResourceApplicationError::UnsupportedOperation)?;
            let key = idempotency_key
                .map(str::to_owned)
                .unwrap_or_else(|| format!("native:volume-delete:{id}"));
            let operation_id = Uuid::new_v5(
                &Uuid::NAMESPACE_URL,
                format!("volume:delete:{id}:{key}").as_bytes(),
            );
            let operation = o3k_store::OperationRecord {
                id: operation_id,
                resource_id,
                kind: "lifecycle:delete".into(),
                state: o3k_store::OperationState::Pending,
                provider_operation_id: None,
                error_category: None,
                error_message: None,
            };
            let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(
                &o3k_kernel::Operation::new(
                    operation_id,
                    "volume",
                    action.clone(),
                    auth.principal().id().to_string(),
                    auth.effective_scope().clone(),
                    o3k_kernel::ResourceType::new_unchecked("volume", "volume"),
                    Some(o3k_kernel::ResourceId::new_unchecked(id)),
                    Some(auth.request_id().to_owned()),
                ),
            )
            .map_err(|_| ResourceApplicationError::Internal)?;
            let request_identity = o3k_store::IdempotencyReservationRequest::from_semantics(
                auth.effective_scope().id().as_str(),
                action.to_string(),
                key,
                "volume:volume",
                Some(id),
                &serde_json::json!({"resource_id": id}),
                operation_id,
            )
            .map_err(|_| ResourceApplicationError::Validation)?;
            if self
                .store
                .create_or_replay_canonical_lifecycle_operation(
                    &operation,
                    &canonical,
                    &request_identity,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?
                == o3k_store::CanonicalAcceptanceOutcome::Conflict
            {
                return Err(ResourceApplicationError::IdempotencyConflict);
            }
            self.store
                .update_resource(
                    resource_id,
                    resource.generation,
                    "DELETED",
                    "DELETED",
                    resource.generation,
                    None,
                )
                .await
                .map_err(|_| ResourceApplicationError::Internal)?;
            return Ok(MutationResult {
                operation_id: operation_id.to_string(),
                resource_id: Some(id.to_owned()),
                complete: true,
                resource: None,
            });
        }
        if descriptor.resource_type.to_string() == "network:network" {
            let resource_id = id
                .parse::<Uuid>()
                .map_err(|_| ResourceApplicationError::NotFound)?;
            self.network_service
                .delete_network(auth, resource_id)
                .await
                .map_err(|_| ResourceApplicationError::NotFound)?;
            return Ok(MutationResult {
                operation_id: Uuid::new_v5(
                    &Uuid::NAMESPACE_URL,
                    format!("network:delete:{id}").as_bytes(),
                )
                .to_string(),
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

/// Generic O3K-side composition handler. It is intentionally unaware of the
/// composing service's business vocabulary: the manifest registry supplies
/// dependency authority and ResourceApplication supplies child lifecycle.
pub struct CompositionResourceHandler {
    pub application: Arc<dyn ResourceApplication>,
    pub store: Arc<o3k_store::unified::O3kStore>,
    pub manifests: Arc<o3k_kernel::ManifestRegistry>,
    pub delegation_keys: std::collections::HashMap<String, ed25519_dalek::VerifyingKey>,
    pub dispatcher: o3k_native_api::resource::ResourceDispatcher,
}

impl CompositionResourceHandler {
    async fn validate_relationship(
        &self,
        parent_id: Uuid,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
        child: &o3k_kernel::ResourceReference,
        require_exclusive: bool,
    ) -> Result<o3k_store::ResourceRelationshipRecord, o3k_service_sdk::composition::CompositionError>
    {
        let relationship = self
            .store
            .get_relationship(parent_id, &request.slot)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let child_id = child
            .resource_id
            .as_str()
            .parse::<Uuid>()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if relationship.parent_resource_type != request.parent.resource_type.to_string()
            || relationship.expected_child_resource_type != child.resource_type.to_string()
            || relationship.child_resource_id != Some(child_id)
            || relationship.parent_operation_id != request.parent_operation_id
            || relationship.owner_scope != request.owner_scope.id().as_str()
            || matches!(relationship.state.as_str(), "reserved" | "deleted")
            || (require_exclusive && relationship.ownership != "exclusive")
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        if let Some(child_operation_id) = request.child_operation_id
            && relationship.child_operation_id != Some(child_operation_id)
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        Ok(relationship)
    }

    async fn validate_parent(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<Uuid, o3k_service_sdk::composition::CompositionError> {
        let parent_id: Uuid = request
            .parent
            .resource_id
            .as_str()
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let parent = self
            .store
            .get_resource(parent_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if parent.kind != request.parent.resource_type.to_string()
            || parent.project_id != request.owner_scope.id().as_str()
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let operation = self
            .store
            .get_operation(request.parent_operation_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let canonical = self
            .store
            .get_canonical_operation(request.parent_operation_id)
            .await
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if operation.resource_id != parent_id
            || canonical.resource_id.as_deref() != Some(parent_id.to_string().as_str())
            || canonical.service != request.context.service_id
            || canonical.action != request.context.action.to_string()
            || canonical.owner_scope != request.owner_scope.id().as_str()
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        Ok(parent_id)
    }

    fn authenticate(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<o3k_kernel::AuthContext, o3k_service_sdk::composition::CompositionError> {
        let claims = o3k_service_sdk::verify_wire_delegation(
            &request.delegation,
            &self.delegation_keys,
            SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?
                .as_millis() as u64,
        )
        .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if claims.original_actor.trim().is_empty()
            || claims.owner_scope != request.owner_scope.to_string()
            || claims.operation_id != request.parent_operation_id
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let (kind, id) = claims
            .owner_scope
            .split_once(':')
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if kind != "project" {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new(id)
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
            None,
            None,
        );
        let request_id = claims.request_id.to_string();
        Ok(o3k_kernel::AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new(claims.original_actor.clone())
                    .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
                claims.original_actor,
                None,
            )),
            scope,
            Vec::new(),
            claims.issued_at_unix_ms / 1000,
            claims.expires_at_unix_ms / 1000,
            request.context.audit_correlation.clone(),
            request_id,
            Some(o3k_kernel::ServicePrincipal::new(
                o3k_kernel::PrincipalId::new(request.service_principal.clone())
                    .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
                request.service_principal.clone(),
                request.context.service_id.clone(),
            )),
        ))
    }

    fn dependency_allowed(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<(), o3k_service_sdk::composition::CompositionError> {
        let Some(manifest) = self.manifests.get(&request.context.service_id) else {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        };
        let expected_principal = manifest
            .controller
            .as_ref()
            .and_then(|controller| controller.service_principal.as_deref())
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if expected_principal != request.service_principal
            || request.parent.resource_type.namespace() != manifest.namespace
        {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let resource = request.resource_type.to_string();
        let action = request.action.to_string();
        let declared = manifest.dependencies.iter().any(|dependency| {
            (dependency.kind == o3k_kernel::manifest::DependencyKind::ResourceType
                && dependency.name == resource)
                || (dependency.kind == o3k_kernel::manifest::DependencyKind::Action
                    && dependency.name == action)
        });
        if declared && self.manifests.has_action(&request.action) {
            Ok(())
        } else {
            Err(o3k_service_sdk::composition::CompositionError::Unauthorized)
        }
    }

    fn relationship_record(
        &self,
        request: &o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<o3k_store::ResourceRelationshipRecord, o3k_service_sdk::composition::CompositionError>
    {
        Ok(o3k_store::ResourceRelationshipRecord {
            parent_resource_id: request
                .parent
                .resource_id
                .as_str()
                .parse()
                .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?,
            parent_resource_type: request.parent.resource_type.to_string(),
            slot: request.slot.clone(),
            expected_child_resource_type: request.resource_type.to_string(),
            child_resource_id: request
                .child
                .as_ref()
                .and_then(|child| child.resource_id.as_str().parse().ok()),
            ownership: "exclusive".to_owned(),
            parent_operation_id: request.parent_operation_id,
            child_operation_id: request.child_operation_id,
            owner_scope: request.owner_scope.id().as_str().to_owned(),
            state: "reserved".to_owned(),
            fingerprint: request.context.replay_identity.clone(),
        })
    }

    fn descriptor_for(
        &self,
        resource_type: &o3k_kernel::ResourceType,
    ) -> Result<
        o3k_native_api::resource::ResourceDescriptor,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dispatcher
            .resolve_resource_type(resource_type)
            .cloned()
            .ok_or_else(|| {
                o3k_service_sdk::composition::CompositionError::Failed(format!(
                    "child resource is not registered: {resource_type}"
                ))
            })
    }
}

#[async_trait::async_trait]
impl o3k_service_sdk::composition::CompositionHandler for CompositionResourceHandler {
    async fn create_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        o3k_service_sdk::composition::ChildResourceReceipt,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dependency_allowed(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(format!(
                "dependency denied for {}",
                request.action
            ))
        })?;
        let auth = self.authenticate(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("delegation denied".into())
        })?;
        let parent_id = self.validate_parent(&request).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("parent denied".into())
        })?;
        let descriptor = self.descriptor_for(&request.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Create)
            .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                "child create operation is not declared".into(),
            ))?;
        if expected_action != &request.action {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        let relationship = self
            .store
            .reserve_relationship(&self.relationship_record(&request)?)
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship reservation failed".into(),
                )
            })?;
        // A durable relationship intent is not an empty slot. If a previous
        // create has an operation identity, or the slot is already uncertain
        // or deleting, recovery must observe the canonical operation before
        // another mutation can be attempted.
        if relationship.child_resource_id.is_none()
            && (relationship.child_operation_id.is_some()
                || matches!(relationship.state.as_str(), "unknown" | "deleting"))
        {
            return Err(o3k_service_sdk::composition::CompositionError::UnknownOutcome);
        }
        if let (Some(child), Some(operation_id)) = (
            relationship.child_resource_id,
            relationship.child_operation_id,
        ) {
            return Ok(o3k_service_sdk::composition::ChildResourceReceipt {
                resource: o3k_kernel::ResourceReference {
                    resource_type: request.resource_type,
                    resource_id: o3k_kernel::ResourceId::new(child.to_string()).map_err(|_| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid child id".into(),
                        )
                    })?,
                    generation: 1,
                },
                operation_id,
                owner_scope: request.owner_scope,
                ownership: o3k_kernel::RelationshipOwnership::Exclusive,
            });
        }
        let result = self
            .application
            .create(
                &descriptor,
                &auth,
                o3k_native_api::resource::CreateRequest {
                    api_version: Some("o3k.io/v1".into()),
                    kind: Some(request.resource_type.to_string()),
                    spec: request.desired_spec,
                },
                Some(&request.idempotency_key),
            )
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed("child create failed".into())
            })?;
        let child_id = result
            .resource_id
            .ok_or(o3k_service_sdk::composition::CompositionError::UnknownOutcome)?
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?;
        let child_operation_id = result
            .operation_id
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?;
        let bound = self
            .store
            .bind_relationship(parent_id, &request.slot, child_id, child_operation_id)
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship bind failed".into(),
                )
            })?;
        Ok(o3k_service_sdk::composition::ChildResourceReceipt {
            resource: o3k_kernel::ResourceReference {
                resource_type: request.resource_type,
                resource_id: o3k_kernel::ResourceId::new(child_id.to_string()).map_err(|_| {
                    o3k_service_sdk::composition::CompositionError::Failed(
                        "invalid child id".into(),
                    )
                })?,
                generation: 1,
            },
            operation_id: bound.child_operation_id.unwrap_or(child_operation_id),
            owner_scope: request.owner_scope,
            ownership: o3k_kernel::RelationshipOwnership::Exclusive,
        })
    }

    async fn observe_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<serde_json::Value, o3k_service_sdk::composition::CompositionError> {
        let auth = self.authenticate(&request)?;
        let parent_id = self.validate_parent(&request).await?;
        let child =
            request
                .child
                .clone()
                .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                    "missing child reference".into(),
                ))?;
        let descriptor = self.descriptor_for(&child.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Show)
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let manifest = self
            .manifests
            .get(&request.context.service_id)
            .ok_or(o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        if !manifest.dependencies.iter().any(|dependency| {
            dependency.kind == o3k_kernel::manifest::DependencyKind::Action
                && dependency.name == expected_action.to_string()
        }) {
            return Err(o3k_service_sdk::composition::CompositionError::Unauthorized);
        }
        self.validate_relationship(parent_id, &request, &child, false)
            .await?;
        self.application
            .show(&descriptor, &auth, child.resource_id.as_str())
            .await
            .map_err(|error| {
                o3k_service_sdk::composition::CompositionError::Failed(format!(
                    "child observation failed for {} {}: {error:?}",
                    child.resource_type, child.resource_id
                ))
            })
    }

    async fn delete_child(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        o3k_service_sdk::composition::ChildResourceReceipt,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.dependency_allowed(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(format!(
                "delete dependency denied for {}",
                request.action
            ))
        })?;
        let auth = self.authenticate(&request).map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(
                "delete delegation denied".into(),
            )
        })?;
        let parent_id = self.validate_parent(&request).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed("delete parent denied".into())
        })?;
        let child = request.child.clone().ok_or_else(|| {
            o3k_service_sdk::composition::CompositionError::Failed("delete child missing".into())
        })?;
        self.validate_relationship(parent_id, &request, &child, true)
            .await?;
        let descriptor = self.descriptor_for(&child.resource_type)?;
        let expected_action = descriptor
            .lifecycle_actions
            .get(&o3k_native_api::resource::LifecycleOperation::Delete)
            .ok_or(o3k_service_sdk::composition::CompositionError::Failed(
                "child delete operation is not declared".into(),
            ))?;
        if expected_action != &request.action {
            return Err(o3k_service_sdk::composition::CompositionError::Failed(
                format!(
                    "delete action mismatch expected={} actual={}",
                    expected_action, request.action
                ),
            ));
        }
        self.store
            .set_relationship_state(parent_id, &request.slot, "deleting")
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship state update failed".into(),
                )
            })?;
        let result = self
            .application
            .delete(
                &descriptor,
                &auth,
                child.resource_id.as_str(),
                Some(&request.idempotency_key),
            )
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed("child delete failed".into())
            })?;
        if !result.complete {
            // An accepted child delete is not proof of absence.  A read that
            // proves NotFound is the only safe fast path; every other result
            // remains recoverable as unknown.
            match self
                .application
                .show(&descriptor, &auth, child.resource_id.as_str())
                .await
            {
                Err(ResourceApplicationError::NotFound) => {}
                _ => {
                    self.store
                        .set_relationship_state(parent_id, &request.slot, "unknown")
                        .await
                        .map_err(|_| {
                            o3k_service_sdk::composition::CompositionError::Failed(
                                "relationship state update failed".into(),
                            )
                        })?;
                    return Err(o3k_service_sdk::composition::CompositionError::UnknownOutcome);
                }
            }
        }
        self.store
            .set_relationship_state(parent_id, &request.slot, "deleted")
            .await
            .map_err(|_| {
                o3k_service_sdk::composition::CompositionError::Failed(
                    "relationship state update failed".into(),
                )
            })?;
        Ok(o3k_service_sdk::composition::ChildResourceReceipt {
            resource: child,
            operation_id: result
                .operation_id
                .parse()
                .map_err(|_| o3k_service_sdk::composition::CompositionError::UnknownOutcome)?,
            owner_scope: request.owner_scope,
            ownership: o3k_kernel::RelationshipOwnership::Exclusive,
        })
    }

    async fn list_relationships(
        &self,
        request: o3k_service_sdk::composition::ChildResourceRequest,
    ) -> Result<
        Vec<o3k_service_sdk::composition::RelationshipView>,
        o3k_service_sdk::composition::CompositionError,
    > {
        self.authenticate(&request)?;
        self.validate_parent(&request).await?;
        let parent = request
            .parent
            .resource_id
            .as_str()
            .parse()
            .map_err(|_| o3k_service_sdk::composition::CompositionError::Unauthorized)?;
        let records = self.store.list_relationships(parent).await.map_err(|_| {
            o3k_service_sdk::composition::CompositionError::Failed(
                "relationship listing failed".into(),
            )
        })?;
        records
            .into_iter()
            .map(|record| {
                let (namespace, name) = record
                    .expected_child_resource_type
                    .split_once(':')
                    .ok_or_else(|| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid relationship resource type".into(),
                        )
                    })?;
                let resource_type =
                    o3k_kernel::ResourceType::new(namespace, name).map_err(|_| {
                        o3k_service_sdk::composition::CompositionError::Failed(
                            "invalid relationship resource type".into(),
                        )
                    })?;
                let resource = record
                    .child_resource_id
                    .map(|id| {
                        Ok::<_, o3k_service_sdk::composition::CompositionError>(
                            o3k_kernel::ResourceReference {
                                resource_type: resource_type.clone(),
                                resource_id: o3k_kernel::ResourceId::new(id.to_string()).map_err(
                                    |_| {
                                        o3k_service_sdk::composition::CompositionError::Failed(
                                            "invalid relationship resource id".into(),
                                        )
                                    },
                                )?,
                                generation: 1,
                            },
                        )
                    })
                    .transpose()?;
                Ok(o3k_service_sdk::composition::RelationshipView {
                    slot: record.slot,
                    resource,
                    resource_type,
                    ownership: if record.ownership == "referenced" {
                        o3k_kernel::RelationshipOwnership::Referenced
                    } else {
                        o3k_kernel::RelationshipOwnership::Exclusive
                    },
                    state: record.state,
                    parent_operation_id: record.parent_operation_id,
                    child_operation_id: record.child_operation_id,
                })
            })
            .collect()
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
            network: Arc::new(NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::empty()),
            }),
            external_controllers: Arc::new(BTreeMap::new()),
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
    async fn native_compute_idempotency_isolated_between_owner_scopes() {
        let (router, store, provider) = setup().await;
        let body_a = serde_json::json!({"spec":{"name":"tenant-a","image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}});
        let body_b = serde_json::json!({"spec":{"name":"tenant-b","image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}});

        let (status_a, first_a) = exec(
            &router,
            authed_post("/compute/servers", "a", "shared-key", body_a.clone()),
        )
        .await;
        assert_eq!(status_a, StatusCode::CREATED);
        let (_, replay_a) = exec(
            &router,
            authed_post("/compute/servers", "a", "shared-key", body_a),
        )
        .await;
        assert_eq!(replay_a["resource_id"], first_a["resource_id"]);
        assert_eq!(replay_a["operation_id"], first_a["operation_id"]);

        let conflict_a = exec(&router, authed_post("/compute/servers", "a", "shared-key", serde_json::json!({"spec":{"name":"other-a","image_id":"image-b","flavor_id":"00000000-0000-0000-0000-000000000002","network_ids":["net-b"]}}))).await;
        assert_eq!(conflict_a.0, StatusCode::CONFLICT);

        let (status_b, first_b) = exec(
            &router,
            authed_post("/compute/servers", "b", "shared-key", body_b.clone()),
        )
        .await;
        assert_eq!(status_b, StatusCode::CREATED, "tenant B create: {first_b}");
        assert_ne!(first_b["resource_id"], first_a["resource_id"]);
        assert_ne!(first_b["operation_id"], first_a["operation_id"]);
        let b_resource = store
            .get_resource(uuid::Uuid::parse_str(first_b["resource_id"].as_str().unwrap()).unwrap())
            .await
            .expect("tenant B resource");
        assert_eq!(b_resource.project_id, "project-b");

        let (_, replay_b) = exec(
            &router,
            authed_post("/compute/servers", "b", "shared-key", body_b),
        )
        .await;
        assert_eq!(replay_b["resource_id"], first_b["resource_id"]);
        assert_eq!(replay_b["operation_id"], first_b["operation_id"]);
        let conflict_b = exec(&router, authed_post("/compute/servers", "b", "shared-key", serde_json::json!({"spec":{"name":"other-b","image_id":"image-b","flavor_id":"00000000-0000-0000-0000-000000000002","network_ids":["net-b"]}}))).await;
        assert_eq!(conflict_b.0, StatusCode::CONFLICT);

        assert_eq!(
            exec(
                &router,
                authed(
                    &format!(
                        "/compute/servers/{}",
                        first_a["resource_id"].as_str().unwrap()
                    ),
                    "b"
                )
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            exec(
                &router,
                authed(
                    &format!("/operations/{}", first_a["operation_id"].as_str().unwrap()),
                    "b"
                )
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(provider.instance_count(), 2);
    }

    #[tokio::test]
    async fn native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation() {
        let (router, _, provider) = setup().await;
        let body = serde_json::json!({
            "spec": {
                "name": "security-test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let malformed = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Basic not-a-bearer")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);

        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Bearer invalid-token-is-not-used")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(provider.instance_count(), 0);
        let (_, created) = exec(
            &router,
            authed_post("/compute/servers", "a", "isolated", body),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap();
        let operation_id = created["operation_id"].as_str().unwrap();

        let foreign_show = exec(
            &router,
            authed(&format!("/compute/servers/{resource_id}"), "b"),
        )
        .await;
        assert_eq!(foreign_show.0, StatusCode::FORBIDDEN);
        let foreign_operation =
            exec(&router, authed(&format!("/operations/{operation_id}"), "b")).await;
        assert_eq!(foreign_operation.0, StatusCode::NOT_FOUND);

        let unknown = exec(&router, authed("/unknown/servers", "a")).await;
        assert_eq!(unknown.0, StatusCode::NOT_FOUND);

        let cross_scope_replay = exec(
            &router,
            authed_post(
                "/compute/servers",
                "b",
                "isolated",
                serde_json::json!({"spec":{"name":"different"}}),
            ),
        )
        .await;
        assert_eq!(cross_scope_replay.0, StatusCode::BAD_REQUEST);
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
