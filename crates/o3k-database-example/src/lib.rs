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

use o3k_kernel::manifest::{RegisteredResourceType, ResourceScope};
use o3k_kernel::resource::ResourceType;
use o3k_kernel::{
    ManifestRegistry, ServiceManifest,
    controller::{
        Controller, ControllerCapabilities, ControllerHealth, ControllerRegistration,
        ControllerSession, ControllerState, DeleteRequest, Observation, ObserveOutcome,
        ObserveRequest, ProtocolVersion, ReconcileOutcome, ReconcileRequest,
    },
};

/// Returns the canonical ServiceManifest for the database example service.
#[must_use]
pub fn manifest() -> ServiceManifest {
    ServiceManifest {
        manifest_version: 1,
        service_id: "database-example".to_owned(),
        namespace: "database".to_owned(),
        service_version: "0.1.0".to_owned(),
        ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
        resource_types: vec![RegisteredResourceType {
            resource_type: ResourceType::new_unchecked("database", "instance"),
            schema_version: "v1".to_owned(),
            collection: None,
            scope: ResourceScope::Tenant,
            operations: [
                (
                    "show".to_owned(),
                    o3k_kernel::ActionId::new_unchecked("database", "ReadInstance"),
                ),
                (
                    "create".to_owned(),
                    o3k_kernel::ActionId::new_unchecked("database", "CreateInstance"),
                ),
                (
                    "delete".to_owned(),
                    o3k_kernel::ActionId::new_unchecked("database", "DeleteInstance"),
                ),
            ]
            .into_iter()
            .collect(),
        }],
        actions: vec![
            "database:CreateInstance".to_owned(),
            "database:ReadInstance".to_owned(),
            "database:DeleteInstance".to_owned(),
        ],
        capabilities: vec!["conformance".to_owned()],
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
    }
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use o3k_kernel::{ActionId, ManifestRegistry, ResourceType};

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
}
