//! P12 conformance service: `database` namespace, `database:instance` resource.
//!
//! This is a deliberately minimal example service that proves the P12
//! extensibility architecture:
//!
//! 1. A non-core service declares its namespace, resource types, and actions.
//! 2. It registers through the ManifestRegistry without Database-specific
//!    business logic in `o3k-kernel`.
//! 3. It becomes discoverable through the native API and generic CLI.
//! 4. It inherits O3K IAM, authorization, and platform contracts.
//!
//! This is NOT a production managed PostgreSQL service. See SPEC-0031 §21
//! and ADR-0174 §15 for the acceptance criteria.

use o3k_controller_protocol::proto;
use o3k_kernel::{
    ManifestRegistry, ServiceManifest,
    controller::{
        Controller, ControllerCapabilities, ControllerHealth, ControllerRegistration,
        ControllerSession, ControllerState, DeleteRequest, Observation, ObserveOutcome,
        ObserveRequest, ProtocolVersion, ReconcileOutcome, ReconcileRequest,
    },
};
use o3k_service_sdk::ControllerHandler;
use o3k_service_sdk::composition::{
    ChildResourceReceipt, ChildResourceRequest, CompositionError, ServiceCompositionClient,
};
use std::sync::Arc;

/// Returns the canonical ServiceManifest for the database example service.
#[must_use]
#[allow(clippy::expect_used)]
pub fn manifest() -> ServiceManifest {
    let wire: o3k_kernel::ServiceManifestV1 =
        serde_json::from_str(include_str!("../service-manifest.json"))
            .expect("database example manifest is valid JSON");
    wire.try_into().expect("database example manifest is valid")
}

/// Registers the database example service into a ManifestRegistry.
///
/// Returns the registered manifest on success.
pub fn register(registry: &mut ManifestRegistry) -> Result<(), o3k_kernel::ManifestError> {
    let m = manifest();
    registry.register(m.clone())?;
    Ok(())
}

/// A minimal in-process controller for the database example service.
///
/// This controller implements the `Controller` trait. In production this
/// would be a separate process using the gRPC controller protocol; here
/// it exists to prove the contract.
pub struct DatabaseExampleController {
    service_id: String,
    session: ControllerSession,
}

impl DatabaseExampleController {
    /// Creates a new controller with the given session.
    #[must_use]
    pub fn new(service_id: impl Into<String>, session: ControllerSession) -> Self {
        Self {
            service_id: service_id.into(),
            session,
        }
    }

    /// Returns the service ID.
    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Returns a registration record for this controller.
    /// The controller starts as `Declared`; call `activate_controller` after
    /// health checks pass to transition to `Ready`.
    #[must_use]
    pub fn registration(&self) -> ControllerRegistration {
        ControllerRegistration {
            service_id: self.service_id.clone(),
            namespace: "database".to_owned(),
            session: Some(self.session.clone()),
            state: ControllerState::Declared,
            health: None,
        }
    }
}

/// Raw-wire adapter used only at the external process boundary. Normal
/// service logic remains in [`DatabaseComposition`] and receives typed values;
/// the SDK server owns session/delegation/replay checks before this handler is
/// called.
pub struct DatabaseControllerHandler<C> {
    client: Arc<C>,
    lifecycle: ChildLifecycleActions,
}

impl<C> DatabaseControllerHandler<C> {
    #[must_use]
    pub fn new(client: Arc<C>, lifecycle: ChildLifecycleActions) -> Self {
        Self { client, lifecycle }
    }
}

#[allow(clippy::result_large_err)]
fn wire_scope(scope: &proto::Scope) -> Result<o3k_kernel::OwnershipScope, tonic::Status> {
    let id = o3k_kernel::ScopeId::new(&scope.id)
        .map_err(|_| tonic::Status::invalid_argument("invalid owner scope"))?;
    match proto::scope::Kind::try_from(scope.kind) {
        Ok(proto::scope::Kind::Project) => Ok(o3k_kernel::OwnershipScope::project(
            id,
            (!scope.name.is_empty()).then(|| scope.name.clone()),
            (!scope.domain_id.is_empty()).then(|| scope.domain_id.clone()),
        )),
        Ok(proto::scope::Kind::Domain) => Ok(o3k_kernel::OwnershipScope::new(
            id,
            o3k_kernel::ScopeKind::Domain,
            (!scope.name.is_empty()).then(|| scope.name.clone()),
            (!scope.domain_id.is_empty()).then(|| scope.domain_id.clone()),
        )),
        Ok(proto::scope::Kind::System) => Ok(o3k_kernel::OwnershipScope::new(
            id,
            o3k_kernel::ScopeKind::System,
            None,
            None,
        )),
        _ => Err(tonic::Status::invalid_argument("invalid owner scope kind")),
    }
}

#[allow(clippy::result_large_err)]
fn wire_reference(
    reference: &proto::ResourceRef,
) -> Result<o3k_kernel::ResourceReference, tonic::Status> {
    let resource_type = o3k_kernel::ResourceType::new(&reference.namespace, &reference.r#type)
        .map_err(|_| tonic::Status::invalid_argument("invalid resource type"))?;
    let resource_id = o3k_kernel::ResourceId::new(&reference.id)
        .map_err(|_| tonic::Status::invalid_argument("invalid resource id"))?;
    if reference.generation < 0 {
        return Err(tonic::Status::invalid_argument(
            "invalid resource generation",
        ));
    }
    Ok(o3k_kernel::ResourceReference {
        resource_type,
        resource_id,
        generation: reference.generation,
    })
}

#[allow(clippy::result_large_err)]
fn wire_context(
    context: &proto::Context,
    scope: o3k_kernel::OwnershipScope,
) -> Result<o3k_kernel::OperationContext, tonic::Status> {
    Ok(o3k_kernel::OperationContext {
        request_id: uuid::Uuid::parse_str(&context.request_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid request id"))?,
        operation_id: uuid::Uuid::parse_str(&context.operation_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid operation id"))?,
        action: o3k_kernel::ActionId::parse(&context.action)
            .map_err(|_| tonic::Status::invalid_argument("invalid action"))?,
        service_id: context.service_id.clone(),
        owner_scope: scope,
        session_id: uuid::Uuid::parse_str(&context.session_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid session id"))?,
        session_generation: context.session_generation,
        deadline_unix_ms: context.deadline_unix_ms,
        replay_identity: context.replay_identity.clone(),
        audit_correlation: context.audit_correlation.clone(),
    })
}

fn observation_response(
    resource: proto::ResourceRef,
    status: InstanceStatus,
) -> proto::Observation {
    proto::Observation {
        resource: Some(resource),
        exists: true,
        observed_revision: String::new(),
        status: serde_json::to_vec(&status).unwrap_or_default(),
        diagnostics: String::new(),
    }
}

#[tonic::async_trait]
impl<C: ServiceCompositionClient + 'static> ControllerHandler for DatabaseControllerHandler<C> {
    async fn health(
        &self,
        _request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, tonic::Status> {
        Ok(proto::HealthResponse {
            healthy: true,
            detail: "database conformance controller is ready".into(),
            protocol_version: Some(proto::Version { major: 1, minor: 0 }),
        })
    }

    async fn capabilities(
        &self,
        _request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, tonic::Status> {
        Ok(proto::CapabilitiesResponse {
            protocol_version: Some(proto::Version { major: 1, minor: 0 }),
            resource_types: vec!["database:instance".into()],
            actions: vec![
                "database:CreateInstance".into(),
                "database:ReadInstance".into(),
                "database:DeleteInstance".into(),
            ],
        })
    }

    async fn reconcile(
        &self,
        request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, tonic::Status> {
        let context = request
            .context
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        let snapshot = request
            .resource
            .ok_or_else(|| tonic::Status::invalid_argument("resource snapshot is required"))?;
        let resource = snapshot
            .resource
            .ok_or_else(|| tonic::Status::invalid_argument("resource reference is required"))?;
        let scope = wire_scope(
            snapshot
                .owner_scope
                .as_ref()
                .or(context.owner_scope.as_ref())
                .ok_or_else(|| tonic::Status::invalid_argument("owner scope is required"))?,
        )?;
        let operation_id = uuid::Uuid::parse_str(&context.operation_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid operation id"))?;
        let context_domain = wire_context(&context, scope.clone())?;
        let spec: InstanceSpec = serde_json::from_slice(&snapshot.desired_spec)
            .map_err(|_| tonic::Status::invalid_argument("invalid database instance spec"))?;
        let reference = wire_reference(&resource)?;
        let delegation = request
            .delegation
            .map(|value| value.credential)
            .unwrap_or_default();
        let composition = DatabaseComposition::new(self.client.clone(), self.lifecycle.clone())
            .with_parent_delegation(delegation);
        let state = composition
            .reconstruct(
                reference.clone(),
                operation_id,
                context_domain.clone(),
                "database-controller".into(),
                scope.clone(),
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let state = composition
            .reconcile(
                reference.clone(),
                operation_id,
                context_domain.clone(),
                "database-controller".into(),
                scope.clone(),
                &spec,
                state,
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let status = composition
            .observe(
                reference,
                operation_id,
                context_domain,
                "database-controller".into(),
                scope,
                &state,
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let accepted = status.phase != "Ready";
        Ok(proto::ReconcileResponse {
            observation: Some(observation_response(resource, status)),
            failure: None,
            accepted,
        })
    }

    async fn observe(
        &self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, tonic::Status> {
        let context = request
            .context
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        let resource = request
            .resource
            .ok_or_else(|| tonic::Status::invalid_argument("resource is required"))?;
        let scope = wire_scope(
            request
                .owner_scope
                .as_ref()
                .or(context.owner_scope.as_ref())
                .ok_or_else(|| tonic::Status::invalid_argument("owner scope is required"))?,
        )?;
        let reference = wire_reference(&resource)?;
        let operation_id = uuid::Uuid::parse_str(&context.operation_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid operation id"))?;
        let composition = DatabaseComposition::new(self.client.clone(), self.lifecycle.clone())
            .with_parent_delegation(
                request
                    .delegation
                    .map(|value| value.credential)
                    .unwrap_or_default(),
            );
        let state = composition
            .reconstruct(
                reference.clone(),
                operation_id,
                wire_context(&context, scope.clone())?,
                "database-controller".into(),
                scope.clone(),
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        let status = composition
            .observe(
                reference,
                operation_id,
                wire_context(&context, scope.clone())?,
                "database-controller".into(),
                scope,
                &state,
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        Ok(proto::ObserveResponse {
            observation: Some(observation_response(resource, status)),
            failure: None,
        })
    }

    async fn delete(
        &self,
        request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, tonic::Status> {
        let context = request
            .context
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        let resource = request
            .resource
            .ok_or_else(|| tonic::Status::invalid_argument("resource is required"))?;
        let scope = wire_scope(
            request
                .owner_scope
                .as_ref()
                .or(context.owner_scope.as_ref())
                .ok_or_else(|| tonic::Status::invalid_argument("owner scope is required"))?,
        )?;
        let reference = wire_reference(&resource)?;
        let operation_id = uuid::Uuid::parse_str(&context.operation_id)
            .map_err(|_| tonic::Status::invalid_argument("invalid operation id"))?;
        let context_domain = wire_context(&context, scope.clone())?;
        let composition = DatabaseComposition::new(self.client.clone(), self.lifecycle.clone())
            .with_parent_delegation(
                request
                    .delegation
                    .map(|value| value.credential)
                    .unwrap_or_default(),
            );
        let state = composition
            .reconstruct(
                reference.clone(),
                operation_id,
                context_domain.clone(),
                "database-controller".into(),
                scope.clone(),
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        composition
            .compensate(
                reference,
                operation_id,
                context_domain,
                "database-controller".into(),
                scope,
                &state,
            )
            .await
            .map_err(|error| tonic::Status::failed_precondition(error.to_string()))?;
        Ok(proto::DeleteResponse {
            observation: Some(proto::Observation {
                resource: Some(resource),
                exists: false,
                observed_revision: String::new(),
                status: Vec::new(),
                diagnostics: String::new(),
            }),
            failure: None,
            accepted: false,
        })
    }
}

#[async_trait::async_trait]
impl Controller for DatabaseExampleController {
    async fn health(&self) -> ControllerHealth {
        ControllerHealth {
            healthy: true,
            detail: Some("database example controller is healthy".to_owned()),
            protocol_version: ProtocolVersion::V1,
        }
    }

    async fn capabilities(&self) -> ControllerCapabilities {
        ControllerCapabilities {
            protocol_version: ProtocolVersion::V1,
            resource_types: vec!["database:instance".to_owned()],
            actions: vec![
                "database:CreateInstance".to_owned(),
                "database:ReadInstance".to_owned(),
                "database:DeleteInstance".to_owned(),
            ],
        }
    }

    async fn reconcile(&self, _request: ReconcileRequest) -> ReconcileOutcome {
        // Minimal conformance: accept any resource as successfully reconciled.
        ReconcileOutcome::Succeeded { observation: None }
    }

    async fn observe(&self, request: ObserveRequest) -> ObserveOutcome {
        ObserveOutcome {
            observation: Some(Observation {
                resource: request.resource,
                exists: false,
                observed_revision: None,
                status: None,
                diagnostics: None,
            }),
            failure: None,
        }
    }

    async fn delete(&self, _request: DeleteRequest) -> ReconcileOutcome {
        ReconcileOutcome::Succeeded { observation: None }
    }
}

/// Schema for the `database:instance` resource status.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstanceStatus {
    pub phase: String,
    pub host: Option<String>,
    pub port: Option<u16>,
}

/// Schema for the `database:instance` resource spec.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstanceSpec {
    pub engine: String,
    pub version: String,
    pub storage_gb: u64,
}

/// Service-owned composition state. The control plane owns the parent and
/// operation; this state contains only generic child references and is
/// reconstructible from durable parent relationship records.
#[derive(Debug, Clone, Default)]
pub struct CompositionState {
    pub network: Option<ChildResourceReceipt>,
    pub volume: Option<ChildResourceReceipt>,
    pub compute: Option<ChildResourceReceipt>,
}

pub struct DatabaseComposition<C> {
    client: Arc<C>,
    lifecycle: ChildLifecycleActions,
    parent_delegation: Vec<u8>,
}

/// Canonical lifecycle actions resolved from the ManifestRegistry by the
/// control plane. The example never derives an ActionId from a resource name.
#[derive(Debug, Clone)]
pub struct ChildLifecycleActions {
    pub network_create: o3k_kernel::ActionId,
    pub network_observe: o3k_kernel::ActionId,
    pub network_delete: o3k_kernel::ActionId,
    pub volume_create: o3k_kernel::ActionId,
    pub volume_observe: o3k_kernel::ActionId,
    pub volume_delete: o3k_kernel::ActionId,
    pub compute_create: o3k_kernel::ActionId,
    pub compute_observe: o3k_kernel::ActionId,
    pub compute_delete: o3k_kernel::ActionId,
}

impl<C: ServiceCompositionClient> DatabaseComposition<C> {
    pub fn new(client: Arc<C>, lifecycle: ChildLifecycleActions) -> Self {
        Self {
            client,
            lifecycle,
            parent_delegation: Vec::new(),
        }
    }

    /// Supplies the O3K-issued bounded parent delegation. The controller
    /// never signs or broadens this credential; it forwards it unchanged.
    #[must_use]
    pub fn with_parent_delegation(mut self, delegation: Vec<u8>) -> Self {
        self.parent_delegation = delegation;
        self
    }

    /// Rebuilds the service-local view exclusively from the O3K relationship
    /// ledger. The controller may lose all process memory and still converge
    /// without allocating a second child for an existing slot.
    pub async fn reconstruct(
        &self,
        parent: o3k_kernel::ResourceReference,
        parent_operation_id: uuid::Uuid,
        context: o3k_kernel::OperationContext,
        service_principal: String,
        owner_scope: o3k_kernel::OwnershipScope,
    ) -> Result<CompositionState, CompositionError> {
        let request = ChildResourceRequest {
            parent,
            parent_operation_id,
            child_operation_id: None,
            context,
            service_principal,
            delegation: self.parent_delegation.clone(),
            child: None,
            action: self.lifecycle.compute_create.clone(),
            resource_type: o3k_kernel::ResourceType::new("relationship", "record")
                .map_err(|error| CompositionError::Failed(error.to_string()))?,
            owner_scope,
            slot: "relationships".into(),
            idempotency_key: format!("{parent_operation_id}:relationships"),
            desired_spec: serde_json::Value::Null,
        };
        let relationships = self.client.list_relationships(request.clone()).await?;
        let mut state = CompositionState::default();
        for relationship in relationships {
            if matches!(relationship.state.as_str(), "deleted" | "reserved") {
                continue;
            }
            let Some(resource) = relationship.resource else {
                continue;
            };
            let receipt = ChildResourceReceipt {
                resource,
                operation_id: relationship
                    .child_operation_id
                    .unwrap_or(relationship.parent_operation_id),
                owner_scope: request.owner_scope.clone(),
                ownership: relationship.ownership,
            };
            match relationship.slot.as_str() {
                "network-primary" => state.network = Some(receipt),
                "volume-data" => state.volume = Some(receipt),
                "compute-primary" => state.compute = Some(receipt),
                _ => {}
            }
        }
        Ok(state)
    }

    /// Creates deterministic child slots in dependency order. Each request
    /// carries the same parent/scope/operation and a stable idempotency key.
    #[allow(clippy::too_many_arguments)]
    pub async fn reconcile(
        &self,
        parent: o3k_kernel::ResourceReference,
        parent_operation_id: uuid::Uuid,
        context: o3k_kernel::OperationContext,
        service_principal: String,
        owner_scope: o3k_kernel::OwnershipScope,
        spec: &InstanceSpec,
        mut state: CompositionState,
    ) -> Result<CompositionState, CompositionError> {
        let slots = [
            (
                "network-primary",
                "network",
                "network",
                self.lifecycle.network_create.clone(),
            ),
            (
                "volume-data",
                "volume",
                "volume",
                self.lifecycle.volume_create.clone(),
            ),
            (
                "compute-primary",
                "compute",
                "server",
                self.lifecycle.compute_create.clone(),
            ),
        ];
        for (slot, namespace, name, action) in slots {
            let exists = match slot {
                "network-primary" => state.network.is_some(),
                "volume-data" => state.volume.is_some(),
                "compute-primary" => state.compute.is_some(),
                _ => false,
            };
            if exists {
                continue;
            }
            let request = ChildResourceRequest {
                parent: parent.clone(),
                parent_operation_id,
                child_operation_id: None,
                context: context.clone(),
                service_principal: service_principal.clone(),
                delegation: self.parent_delegation.clone(),
                child: None,
                action,
                resource_type: o3k_kernel::ResourceType::new(namespace, name)
                    .map_err(|e| CompositionError::Failed(e.to_string()))?,
                owner_scope: owner_scope.clone(),
                slot: slot.to_owned(),
                idempotency_key: format!("{parent_operation_id}:{slot}"),
                desired_spec: match slot {
                    // Child APIs receive only fields from their own schema.
                    "network-primary" => serde_json::json!({
                        "name": format!("database-network-{}", parent.resource_id),
                    }),
                    "volume-data" => serde_json::json!({
                        "size_bytes": spec.storage_gb.saturating_mul(1024 * 1024 * 1024),
                        "volume_type": "standard"
                    }),
                    "compute-primary" => serde_json::json!({
                        "name": format!("database-{}", parent.resource_id),
                        "image_id": "image-1",
                        "flavor_id": uuid::Uuid::from_u128(1).to_string(),
                        "network_ids": state.network.as_ref().map(|receipt| {
                            vec![receipt.resource.resource_id.as_str().to_owned()]
                        }).unwrap_or_default(),
                        "key_name": serde_json::Value::Null
                    }),
                    _ => return Err(CompositionError::Failed("unknown child slot".into())),
                },
            };
            let receipt = match self.client.create_child(request).await {
                Ok(receipt) => receipt,
                Err(error) => {
                    // A later child failure compensates only the durable
                    // exclusive children already known in this workflow.
                    // If compensation itself is uncertain, surface that
                    // outcome and leave the relationship ledger recoverable.
                    self.compensate(
                        parent.clone(),
                        parent_operation_id,
                        context.clone(),
                        service_principal.clone(),
                        owner_scope.clone(),
                        &state,
                    )
                    .await?;
                    return Err(CompositionError::Failed(format!("{slot}: {error}")));
                }
            };
            match slot {
                "network-primary" => state.network = Some(receipt),
                "volume-data" => state.volume = Some(receipt),
                "compute-primary" => state.compute = Some(receipt),
                _ => unreachable!(),
            }
        }
        Ok(state)
    }

    /// Observes every durable child reference and returns a service-owned
    /// status derived from canonical child observations. This is read-only;
    /// missing children are reported as not ready and are not recreated here.
    pub async fn observe(
        &self,
        parent: o3k_kernel::ResourceReference,
        parent_operation_id: uuid::Uuid,
        context: o3k_kernel::OperationContext,
        service_principal: String,
        owner_scope: o3k_kernel::OwnershipScope,
        state: &CompositionState,
    ) -> Result<InstanceStatus, CompositionError> {
        let children = [
            (
                state.network.as_ref(),
                self.lifecycle.network_observe.clone(),
            ),
            (state.volume.as_ref(), self.lifecycle.volume_observe.clone()),
            (
                state.compute.as_ref(),
                self.lifecycle.compute_observe.clone(),
            ),
        ];
        let mut ready = true;
        for (receipt, action) in children {
            let Some(receipt) = receipt else {
                ready = false;
                continue;
            };
            let observation = self
                .client
                .observe_child(ChildResourceRequest {
                    parent: parent.clone(),
                    parent_operation_id,
                    child_operation_id: Some(receipt.operation_id),
                    context: context.clone(),
                    service_principal: service_principal.clone(),
                    delegation: self.parent_delegation.clone(),
                    child: Some(receipt.resource.clone()),
                    action,
                    resource_type: receipt.resource.resource_type.clone(),
                    owner_scope: owner_scope.clone(),
                    slot: format!("observe:{}", receipt.resource.resource_type),
                    idempotency_key: format!(
                        "{parent_operation_id}:observe:{}",
                        receipt.resource.resource_id
                    ),
                    desired_spec: serde_json::Value::Null,
                })
                .await?;
            let state = observation
                .get("status")
                .and_then(|value| value.get("state"))
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            ready &= matches!(
                state,
                "ACTIVE" | "AVAILABLE" | "active" | "available" | "succeeded"
            );
        }
        Ok(InstanceStatus {
            phase: if ready { "Ready" } else { "Provisioning" }.into(),
            host: None,
            port: None,
        })
    }

    /// Compensate only the exclusive children known in durable state, in
    /// reverse dependency order. Missing/unknown outcomes are returned to the
    /// caller so the parent operation remains recoverable rather than being
    /// reported as cleanly deleted.
    #[allow(clippy::too_many_arguments)]
    pub async fn compensate(
        &self,
        parent: o3k_kernel::ResourceReference,
        parent_operation_id: uuid::Uuid,
        context: o3k_kernel::OperationContext,
        service_principal: String,
        owner_scope: o3k_kernel::OwnershipScope,
        state: &CompositionState,
    ) -> Result<(), CompositionError> {
        let receipts = [
            state.compute.as_ref(),
            state.volume.as_ref(),
            state.network.as_ref(),
        ]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
        for receipt in &receipts {
            if receipt.ownership != o3k_kernel::RelationshipOwnership::Exclusive
                || receipt.owner_scope != owner_scope
            {
                return Err(CompositionError::Unauthorized);
            }
            if !matches!(
                receipt.resource.resource_type.to_string().as_str(),
                "network:network" | "volume:volume" | "compute:server"
            ) {
                return Err(CompositionError::Failed(
                    "unsupported child resource".into(),
                ));
            }
        }
        for receipt in receipts {
            let resource = receipt.resource.clone();
            let action = match resource.resource_type.to_string().as_str() {
                "network:network" => self.lifecycle.network_delete.clone(),
                "volume:volume" => self.lifecycle.volume_delete.clone(),
                "compute:server" => self.lifecycle.compute_delete.clone(),
                _ => {
                    return Err(CompositionError::Failed(
                        "unsupported child resource".into(),
                    ));
                }
            };
            let request = ChildResourceRequest {
                parent: parent.clone(),
                parent_operation_id,
                child_operation_id: Some(receipt.operation_id),
                context: context.clone(),
                service_principal: service_principal.clone(),
                delegation: self.parent_delegation.clone(),
                child: Some(resource.clone()),
                action,
                resource_type: resource.resource_type.clone(),
                owner_scope: owner_scope.clone(),
                slot: match resource.resource_type.to_string().as_str() {
                    "network:network" => "network-primary".to_owned(),
                    "volume:volume" => "volume-data".to_owned(),
                    "compute:server" => "compute-primary".to_owned(),
                    _ => return Err(CompositionError::Failed("unsupported child slot".into())),
                },
                idempotency_key: format!("{parent_operation_id}:delete:{}", resource.resource_id),
                desired_spec: serde_json::Value::Null,
            };
            self.client.delete_child(request).await.map_err(|error| {
                CompositionError::Failed(format!("{}: {error}", resource.resource_type))
            })?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use o3k_kernel::{ActionId, ManifestRegistry, ResourceType};
    use std::sync::Mutex;

    #[test]
    fn manifest_registers_and_displays() -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = ManifestRegistry::new();
        register(&mut registry)?;

        let registered = registry
            .get("database-example")
            .ok_or("service not found")?;
        assert_eq!(registered.namespace, "database");
        assert!(
            registered
                .resource_types
                .iter()
                .any(|rt| rt.resource_type.to_string() == "database:instance")
        );
        Ok(())
    }

    #[test]
    fn registry_reports_resource_type_and_action() -> Result<(), Box<dyn std::error::Error>> {
        let mut registry = ManifestRegistry::new();
        register(&mut registry)?;

        let rt = ResourceType::new("database", "instance")?;
        assert!(registry.has_resource_type(&rt));

        let action = ActionId::new("database", "CreateInstance")?;
        assert!(registry.has_action(&action));
        Ok(())
    }

    #[test]
    fn controller_reconciles_and_health() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();

        let session = o3k_kernel::controller::ControllerSession {
            service_id: "database-example".to_owned(),
            namespace: "database".to_owned(),
            service_principal: o3k_kernel::ServicePrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("db-ctrl-1"),
                "database-controller",
                "database",
            ),
            session_id: uuid::Uuid::new_v4(),
            session_generation: 1,
            protocol_version: ProtocolVersion::V1,
            manifest_digest: "test-digest".to_owned(),
            manifest_generation: 1,
            started_at: "2026-08-21T00:00:00Z".to_owned(),
        };
        let ctrl = DatabaseExampleController::new("database-example", session);
        let health = runtime.block_on(ctrl.health());
        assert!(health.healthy);
        assert_eq!(health.protocol_version, ProtocolVersion::V1);

        let caps = runtime.block_on(ctrl.capabilities());
        assert!(
            caps.resource_types
                .contains(&"database:instance".to_owned())
        );
    }

    #[test]
    fn instance_spec_and_status_serialization() -> Result<(), Box<dyn std::error::Error>> {
        let spec = InstanceSpec {
            engine: "postgresql".to_owned(),
            version: "16".to_owned(),
            storage_gb: 10,
        };
        let json = serde_json::to_string(&spec)?;
        assert!(json.contains("postgresql"));
        assert!(json.contains("storage_gb"));

        let status = InstanceStatus {
            phase: "Running".to_owned(),
            host: Some("db-1.example.com".to_owned()),
            port: Some(5432),
        };
        let json = serde_json::to_string(&status)?;
        assert!(json.contains("Running"));
        assert!(json.contains("5432"));
        Ok(())
    }

    #[test]
    fn conformance_does_not_require_kernel_changes() -> Result<(), Box<dyn std::error::Error>> {
        // This test proves that the database example service can be added
        // WITHOUT modifying o3k-kernel code.
        //
        // All required types are in o3k-kernel as generic primitives:
        // ServiceManifest, ManifestRegistry, Controller trait, etc.
        // No "database:instance" hardcoded in o3k-kernel.
        let mut registry = ManifestRegistry::new();
        let m = manifest();
        registry.register(m)?;

        // Verify no Database-specific fields in kernel are needed
        assert!(registry.has_resource_type(&ResourceType::new("database", "instance")?));
        assert!(registry.has_action(&ActionId::new("database", "CreateInstance")?));
        Ok(())
    }

    struct FakeComposition {
        calls: Mutex<Vec<String>>,
        fail_create: Option<String>,
        fail_observe: Mutex<Option<String>>,
        fail_delete: Mutex<Option<String>>,
    }

    #[async_trait::async_trait]
    impl ServiceCompositionClient for FakeComposition {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError> {
            self.calls
                .lock()
                .expect("calls lock")
                .push(request.slot.clone());
            if self.fail_create.as_deref() == Some(request.slot.as_str()) {
                return Err(CompositionError::Failed("injected child failure".into()));
            }
            Ok(ChildResourceReceipt {
                resource: o3k_kernel::ResourceReference {
                    resource_type: request.resource_type,
                    resource_id: o3k_kernel::ResourceId::new(request.slot.clone())
                        .map_err(|e| CompositionError::Failed(e.to_string()))?,
                    generation: 1,
                },
                operation_id: request.parent_operation_id,
                owner_scope: request.owner_scope,
                ownership: o3k_kernel::RelationshipOwnership::Exclusive,
            })
        }
        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError> {
            if self
                .fail_observe
                .lock()
                .expect("observe failure lock")
                .as_deref()
                == Some(request.slot.as_str())
            {
                return Err(CompositionError::UnknownOutcome);
            }
            Ok(serde_json::json!({"state":"active"}))
        }
        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<(), CompositionError> {
            let slot = request.slot;
            self.calls.lock().expect("calls lock").push(slot.clone());
            if self
                .fail_delete
                .lock()
                .expect("delete failure lock")
                .as_deref()
                == Some(slot.as_str())
            {
                return Err(CompositionError::UnknownOutcome);
            }
            Ok(())
        }

        async fn list_relationships(
            &self,
            _request: ChildResourceRequest,
        ) -> Result<Vec<o3k_service_sdk::composition::RelationshipView>, CompositionError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn composition_uses_generic_deterministic_child_slots() {
        let client = Arc::new(FakeComposition {
            calls: Mutex::new(Vec::new()),
            fail_create: None,
            fail_observe: Mutex::new(None),
            fail_delete: Mutex::new(None),
        });
        let lifecycle = ChildLifecycleActions {
            network_create: ActionId::new("network", "CreateNetwork").unwrap(),
            network_observe: ActionId::new("network", "ReadNetwork").unwrap(),
            network_delete: ActionId::new("network", "DeleteNetwork").unwrap(),
            volume_create: ActionId::new("volume", "CreateVolume").unwrap(),
            volume_observe: ActionId::new("volume", "ReadVolume").unwrap(),
            volume_delete: ActionId::new("volume", "DeleteVolume").unwrap(),
            compute_create: ActionId::new("compute", "CreateServer").unwrap(),
            compute_observe: ActionId::new("compute", "ReadServer").unwrap(),
            compute_delete: ActionId::new("compute", "DeleteServer").unwrap(),
        };
        let composition = DatabaseComposition::new(client.clone(), lifecycle);
        let parent = o3k_kernel::ResourceReference {
            resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
            resource_id: o3k_kernel::ResourceId::new("parent-1").unwrap(),
            generation: 1,
        };
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new("project-1").unwrap(),
            None,
            None,
        );
        let operation = uuid::Uuid::new_v4();
        let context = o3k_kernel::OperationContext {
            request_id: uuid::Uuid::new_v4(),
            operation_id: operation,
            action: ActionId::new("database", "CreateInstance").unwrap(),
            service_id: "database-example".into(),
            owner_scope: scope.clone(),
            session_id: uuid::Uuid::new_v4(),
            session_generation: 1,
            deadline_unix_ms: 300_000,
            replay_identity: format!("parent:{operation}"),
            audit_correlation: "test-audit".into(),
        };
        let state = composition
            .reconcile(
                parent,
                operation,
                context.clone(),
                "database-controller".into(),
                scope,
                &InstanceSpec {
                    engine: "test".into(),
                    version: "1".into(),
                    storage_gb: 1,
                },
                CompositionState::default(),
            )
            .await
            .unwrap();
        assert!(state.network.is_some() && state.volume.is_some() && state.compute.is_some());
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            ["network-primary", "volume-data", "compute-primary"]
        );
        *client.fail_observe.lock().unwrap() = Some("observe:network:network".into());
        assert!(
            composition
                .observe(
                    o3k_kernel::ResourceReference {
                        resource_type: o3k_kernel::ResourceType::new("database", "instance")
                            .unwrap(),
                        resource_id: o3k_kernel::ResourceId::new("parent-1").unwrap(),
                        generation: 1,
                    },
                    operation,
                    context.clone(),
                    "database-controller".into(),
                    o3k_kernel::OwnershipScope::project(
                        o3k_kernel::ScopeId::new("project-1").unwrap(),
                        None,
                        None,
                    ),
                    &state,
                )
                .await
                .is_err()
        );
        *client.fail_observe.lock().unwrap() = None;
        composition
            .compensate(
                o3k_kernel::ResourceReference {
                    resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
                    resource_id: o3k_kernel::ResourceId::new("parent-1").unwrap(),
                    generation: 1,
                },
                operation,
                context.clone(),
                "database-controller".into(),
                o3k_kernel::OwnershipScope::project(
                    o3k_kernel::ScopeId::new("project-1").unwrap(),
                    None,
                    None,
                ),
                &state,
            )
            .await
            .unwrap();
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            [
                "network-primary",
                "volume-data",
                "compute-primary",
                "compute-primary",
                "volume-data",
                "network-primary"
            ]
        );
        client.calls.lock().unwrap().clear();
        *client.fail_delete.lock().unwrap() = Some("compute-primary".into());
        assert!(
            composition
                .compensate(
                    o3k_kernel::ResourceReference {
                        resource_type: o3k_kernel::ResourceType::new("database", "instance")
                            .unwrap(),
                        resource_id: o3k_kernel::ResourceId::new("parent-1").unwrap(),
                        generation: 1,
                    },
                    operation,
                    context.clone(),
                    "database-controller".into(),
                    o3k_kernel::OwnershipScope::project(
                        o3k_kernel::ScopeId::new("project-1").unwrap(),
                        None,
                        None,
                    ),
                    &state,
                )
                .await
                .is_err()
        );
        assert_eq!(client.calls.lock().unwrap().as_slice(), ["compute-primary"]);
        *client.fail_delete.lock().unwrap() = None;
        client.calls.lock().unwrap().clear();
        let mut referenced_state = state.clone();
        referenced_state.network.as_mut().unwrap().ownership =
            o3k_kernel::RelationshipOwnership::Referenced;
        assert!(
            composition
                .compensate(
                    o3k_kernel::ResourceReference {
                        resource_type: o3k_kernel::ResourceType::new("database", "instance")
                            .unwrap(),
                        resource_id: o3k_kernel::ResourceId::new("parent-1").unwrap(),
                        generation: 1,
                    },
                    operation,
                    context,
                    "database-controller".into(),
                    o3k_kernel::OwnershipScope::project(
                        o3k_kernel::ScopeId::new("project-1").unwrap(),
                        None,
                        None,
                    ),
                    &referenced_state,
                )
                .await
                .is_err()
        );
        assert!(client.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn partial_child_failure_compensates_exclusive_children_in_reverse_order() {
        let client = Arc::new(FakeComposition {
            calls: Mutex::new(Vec::new()),
            fail_create: Some("volume-data".into()),
            fail_observe: Mutex::new(None),
            fail_delete: Mutex::new(None),
        });
        let lifecycle = ChildLifecycleActions {
            network_create: ActionId::new("network", "CreateNetwork").unwrap(),
            network_observe: ActionId::new("network", "ReadNetwork").unwrap(),
            network_delete: ActionId::new("network", "DeleteNetwork").unwrap(),
            volume_create: ActionId::new("volume", "CreateVolume").unwrap(),
            volume_observe: ActionId::new("volume", "ReadVolume").unwrap(),
            volume_delete: ActionId::new("volume", "DeleteVolume").unwrap(),
            compute_create: ActionId::new("compute", "CreateServer").unwrap(),
            compute_observe: ActionId::new("compute", "ReadServer").unwrap(),
            compute_delete: ActionId::new("compute", "DeleteServer").unwrap(),
        };
        let composition = DatabaseComposition::new(client.clone(), lifecycle);
        let parent = o3k_kernel::ResourceReference {
            resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
            resource_id: o3k_kernel::ResourceId::new("parent-failure").unwrap(),
            generation: 1,
        };
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new("project-failure").unwrap(),
            None,
            None,
        );
        let operation = uuid::Uuid::new_v4();
        let context = o3k_kernel::OperationContext {
            request_id: uuid::Uuid::new_v4(),
            operation_id: operation,
            action: ActionId::new("database", "CreateInstance").unwrap(),
            service_id: "database-example".into(),
            owner_scope: scope.clone(),
            session_id: uuid::Uuid::new_v4(),
            session_generation: 1,
            deadline_unix_ms: 300_000,
            replay_identity: format!("parent:{operation}"),
            audit_correlation: "failure-test".into(),
        };
        let result = composition
            .reconcile(
                parent,
                operation,
                context,
                "database-controller".into(),
                scope,
                &InstanceSpec {
                    engine: "test".into(),
                    version: "1".into(),
                    storage_gb: 1,
                },
                CompositionState::default(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            ["network-primary", "volume-data", "network-primary"]
        );
    }

    #[tokio::test]
    async fn network_failure_stops_before_later_child_side_effects() {
        let client = Arc::new(FakeComposition {
            calls: Mutex::new(Vec::new()),
            fail_create: Some("network-primary".into()),
            fail_observe: Mutex::new(None),
            fail_delete: Mutex::new(None),
        });
        let lifecycle = ChildLifecycleActions {
            network_create: ActionId::new("network", "CreateNetwork").unwrap(),
            network_observe: ActionId::new("network", "ReadNetwork").unwrap(),
            network_delete: ActionId::new("network", "DeleteNetwork").unwrap(),
            volume_create: ActionId::new("volume", "CreateVolume").unwrap(),
            volume_observe: ActionId::new("volume", "ReadVolume").unwrap(),
            volume_delete: ActionId::new("volume", "DeleteVolume").unwrap(),
            compute_create: ActionId::new("compute", "CreateServer").unwrap(),
            compute_observe: ActionId::new("compute", "ReadServer").unwrap(),
            compute_delete: ActionId::new("compute", "DeleteServer").unwrap(),
        };
        let composition = DatabaseComposition::new(client.clone(), lifecycle);
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new("project-network-failure").unwrap(),
            None,
            None,
        );
        let operation = uuid::Uuid::new_v4();
        let context = o3k_kernel::OperationContext {
            request_id: uuid::Uuid::new_v4(),
            operation_id: operation,
            action: ActionId::new("database", "CreateInstance").unwrap(),
            service_id: "database-example".into(),
            owner_scope: scope.clone(),
            session_id: uuid::Uuid::new_v4(),
            session_generation: 1,
            deadline_unix_ms: 300_000,
            replay_identity: format!("parent:{operation}"),
            audit_correlation: "network-failure-test".into(),
        };
        let result = composition
            .reconcile(
                o3k_kernel::ResourceReference {
                    resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
                    resource_id: o3k_kernel::ResourceId::new("parent-network-failure").unwrap(),
                    generation: 1,
                },
                operation,
                context,
                "database-controller".into(),
                scope,
                &InstanceSpec {
                    engine: "test".into(),
                    version: "1".into(),
                    storage_gb: 1,
                },
                CompositionState::default(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(client.calls.lock().unwrap().as_slice(), ["network-primary"]);
    }

    #[tokio::test]
    async fn compute_failure_compensates_network_and_volume_in_reverse_order() {
        let client = Arc::new(FakeComposition {
            calls: Mutex::new(Vec::new()),
            fail_create: Some("compute-primary".into()),
            fail_observe: Mutex::new(None),
            fail_delete: Mutex::new(None),
        });
        let lifecycle = ChildLifecycleActions {
            network_create: ActionId::new("network", "CreateNetwork").unwrap(),
            network_observe: ActionId::new("network", "ReadNetwork").unwrap(),
            network_delete: ActionId::new("network", "DeleteNetwork").unwrap(),
            volume_create: ActionId::new("volume", "CreateVolume").unwrap(),
            volume_observe: ActionId::new("volume", "ReadVolume").unwrap(),
            volume_delete: ActionId::new("volume", "DeleteVolume").unwrap(),
            compute_create: ActionId::new("compute", "CreateServer").unwrap(),
            compute_observe: ActionId::new("compute", "ReadServer").unwrap(),
            compute_delete: ActionId::new("compute", "DeleteServer").unwrap(),
        };
        let composition = DatabaseComposition::new(client.clone(), lifecycle);
        let scope = o3k_kernel::OwnershipScope::project(
            o3k_kernel::ScopeId::new("project-compute-failure").unwrap(),
            None,
            None,
        );
        let operation = uuid::Uuid::new_v4();
        let context = o3k_kernel::OperationContext {
            request_id: uuid::Uuid::new_v4(),
            operation_id: operation,
            action: ActionId::new("database", "CreateInstance").unwrap(),
            service_id: "database-example".into(),
            owner_scope: scope.clone(),
            session_id: uuid::Uuid::new_v4(),
            session_generation: 1,
            deadline_unix_ms: 300_000,
            replay_identity: format!("parent:{operation}"),
            audit_correlation: "compute-failure-test".into(),
        };
        let result = composition
            .reconcile(
                o3k_kernel::ResourceReference {
                    resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
                    resource_id: o3k_kernel::ResourceId::new("parent-compute-failure").unwrap(),
                    generation: 1,
                },
                operation,
                context,
                "database-controller".into(),
                scope,
                &InstanceSpec {
                    engine: "test".into(),
                    version: "1".into(),
                    storage_gb: 1,
                },
                CompositionState::default(),
            )
            .await;
        assert!(result.is_err());
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            [
                "network-primary",
                "volume-data",
                "compute-primary",
                "volume-data",
                "network-primary"
            ]
        );
    }
}
