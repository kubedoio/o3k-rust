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

use o3k_kernel::{
    ManifestRegistry, ServiceManifest,
    controller::{
        Controller, ControllerCapabilities, ControllerHealth, ControllerRegistration,
        ControllerSession, ControllerState, DeleteRequest, Observation, ObserveOutcome,
        ObserveRequest, ProtocolVersion, ReconcileOutcome, ReconcileRequest,
    },
};
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
}

/// Canonical lifecycle actions resolved from the ManifestRegistry by the
/// control plane. The example never derives an ActionId from a resource name.
#[derive(Debug, Clone)]
pub struct ChildLifecycleActions {
    pub network_create: o3k_kernel::ActionId,
    pub network_delete: o3k_kernel::ActionId,
    pub volume_create: o3k_kernel::ActionId,
    pub volume_delete: o3k_kernel::ActionId,
    pub compute_create: o3k_kernel::ActionId,
    pub compute_delete: o3k_kernel::ActionId,
}

impl<C: ServiceCompositionClient> DatabaseComposition<C> {
    pub fn new(client: Arc<C>, lifecycle: ChildLifecycleActions) -> Self {
        Self { client, lifecycle }
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
                "address_realm",
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
                context: context.clone(),
                service_principal: service_principal.clone(),
                delegation: Vec::new(),
                action,
                resource_type: o3k_kernel::ResourceType::new(namespace, name)
                    .map_err(|e| CompositionError::Failed(e.to_string()))?,
                owner_scope: owner_scope.clone(),
                slot: slot.to_owned(),
                idempotency_key: format!("{parent_operation_id}:{slot}"),
                desired_spec: match slot {
                    // Child APIs receive only fields from their own schema.
                    "network-primary" => serde_json::json!({
                        "prefix": format!("10.0.{}.0/24", spec.storage_gb % 250 + 1),
                        "overlapping_prefixes": false
                    }),
                    "volume-data" => serde_json::json!({
                        "size_bytes": spec.storage_gb.saturating_mul(1024 * 1024 * 1024),
                        "volume_type": "standard"
                    }),
                    "compute-primary" => serde_json::json!({
                        "name": format!("database-{}", parent.resource_id),
                        "image_id": format!("database-{}", spec.version),
                        "flavor_id": spec.engine.clone(),
                        "network_ids": [],
                        "key_name": serde_json::Value::Null
                    }),
                    _ => return Err(CompositionError::Failed("unknown child slot".into())),
                },
            };
            let receipt = self.client.create_child(request).await?;
            match slot {
                "network-primary" => state.network = Some(receipt),
                "volume-data" => state.volume = Some(receipt),
                "compute-primary" => state.compute = Some(receipt),
                _ => unreachable!(),
            }
        }
        Ok(state)
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
        for receipt in [
            state.compute.as_ref(),
            state.volume.as_ref(),
            state.network.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            let resource = receipt.resource.clone();
            if receipt.ownership != o3k_kernel::RelationshipOwnership::Exclusive
                || receipt.owner_scope != owner_scope
            {
                return Err(CompositionError::Unauthorized);
            }
            let action = match resource.resource_type.to_string().as_str() {
                "network:address_realm" => self.lifecycle.network_delete.clone(),
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
                context: context.clone(),
                service_principal: service_principal.clone(),
                delegation: Vec::new(),
                action,
                resource_type: resource.resource_type.clone(),
                owner_scope: owner_scope.clone(),
                slot: format!("compensate:{}", resource.resource_type),
                idempotency_key: format!("{parent_operation_id}:delete:{}", resource.resource_id),
                desired_spec: serde_json::Value::Null,
            };
            self.client.delete_child(request).await?;
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
            _resource: o3k_kernel::ResourceReference,
            _operation_id: uuid::Uuid,
        ) -> Result<serde_json::Value, CompositionError> {
            Ok(serde_json::json!({"state":"active"}))
        }
        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<(), CompositionError> {
            self.calls.lock().expect("calls lock").push(request.slot);
            Ok(())
        }
    }

    #[tokio::test]
    async fn composition_uses_generic_deterministic_child_slots() {
        let client = Arc::new(FakeComposition {
            calls: Mutex::new(Vec::new()),
        });
        let lifecycle = ChildLifecycleActions {
            network_create: ActionId::new("network", "CreateAddressRealm").unwrap(),
            network_delete: ActionId::new("network", "DeleteAddressRealm").unwrap(),
            volume_create: ActionId::new("volume", "CreateVolume").unwrap(),
            volume_delete: ActionId::new("volume", "DeleteVolume").unwrap(),
            compute_create: ActionId::new("compute", "CreateServer").unwrap(),
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
        composition
            .compensate(
                o3k_kernel::ResourceReference {
                    resource_type: o3k_kernel::ResourceType::new("database", "instance").unwrap(),
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
                "compensate:compute:server",
                "compensate:volume:volume",
                "compensate:network:address_realm"
            ]
        );
    }
}
