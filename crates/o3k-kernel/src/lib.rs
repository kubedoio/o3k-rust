//! O3K Cloud Kernel: the foundational IAM, authorization, resource ownership,
//! service registry, and platform audit contracts shared across all first-class
//! O3K cloud services.
//!
//! Architectural invariants (ADR-0165, ADR-0166, SPEC-0020):
//! - Inward-facing core crate: must never depend on API, store, identity, provider,
//!   or framework crates.
//! - Canonical IAM & AuthContext: single authoritative `AuthContext` consumed by
//!   all application services.
//! - Service-neutral authorization: `Principal × Action × Resource × Context -> Allow/Deny`.
//! - Default-deny fail-closed policy model.
//! - Canonical static service registry with Keystone catalog projection.
//! - Canonical secret-safe audit events and bounded audit sink port.

pub mod action;
pub mod audit;
pub mod auth_context;
pub mod authorization;
pub mod error;
pub mod principal;
pub mod quota;
pub mod registry;
pub mod resource;
pub mod scope;

pub use action::ActionId;
pub use audit::{
    AuditEvent, AuditOutcome, AuditSink, EventId, FnAuditSink, MemoryAuditSink, NoopAuditSink,
};
pub use auth_context::AuthContext;
pub use authorization::{
    ActionPolicy, AuthorizationDecision, AuthorizationRequest, Authorizer, DecisionReason,
    StaticAuthorizer,
};
pub use error::KernelError;
pub use principal::{Principal, PrincipalId, PrincipalKind, ServicePrincipal, UserPrincipal};
pub use quota::{
    LimitKey, LimitValue, QuotaDecision, Reservation, ReservationId, ReservationState,
    ResourceAmount, Usage,
};
pub use registry::{
    ApiSurface, EndpointTemplate, KernelRegistry, KeystoneCatalogEndpoint, KeystoneCatalogService,
    ServiceDescriptor, ServiceId, ServiceNamespace, ServiceOwnership,
};
pub use resource::{ResourceId, ResourceTarget, ResourceType};
pub use scope::{OwnershipScope, ScopeId, ScopeKind};

#[cfg(test)]
mod tests {
    use super::*;

    fn test_user_context(user_id: &str, project_id: &str) -> AuthContext {
        let principal_id = PrincipalId::new_unchecked(user_id);
        let user = UserPrincipal::new(principal_id, "test-user", Some("default".to_string()));
        let scope_id = ScopeId::new_unchecked(project_id);
        let scope = OwnershipScope::project(
            scope_id,
            Some("test-project".to_string()),
            Some("default".to_string()),
        );
        AuthContext::new(
            Principal::User(user),
            scope,
            vec!["member".to_string(), "reader".to_string()],
            1700000000,
            1700003600,
            "audit-12345",
            "req-67890",
            None,
        )
    }

    fn test_service_context(service_id: &str, project_id: &str) -> AuthContext {
        let principal_id = PrincipalId::new_unchecked(service_id);
        let service = ServicePrincipal::new(principal_id, "cinder", "volumev3");
        let scope_id = ScopeId::new_unchecked(project_id);
        let scope = OwnershipScope::project(
            scope_id,
            Some("service-project".to_string()),
            Some("default".to_string()),
        );
        AuthContext::new(
            Principal::Service(service.clone()),
            scope,
            vec!["service".to_string(), "admin".to_string()],
            1700000000,
            1700003600,
            "audit-67890",
            "req-12345",
            Some(service),
        )
    }

    #[test]
    fn authorizer_standard_allow_owner() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::instance(
            ResourceType::new("compute", "server")?,
            ResourceId::new("srv-1")?,
            Some(ScopeId::new("proj-1")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "ReadServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::Allowed);
        Ok(())
    }

    #[test]
    fn authorizer_standard_deny_cross_project() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::instance(
            ResourceType::new("compute", "server")?,
            ResourceId::new("srv-1")?,
            Some(ScopeId::new("proj-2")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "ReadServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::ScopeMismatch);
        Ok(())
    }

    #[test]
    fn authorizer_deny_unknown_action() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(
            ResourceType::new("compute", "server")?,
            Some(ScopeId::new("proj-1")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "NonExistentAction")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::UnknownAction);
        Ok(())
    }

    #[test]
    fn authorizer_deny_unknown_resource_type() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(
            ResourceType::new("database", "instance")?,
            Some(ScopeId::new("proj-1")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "CreateServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::UnknownResourceType);
        Ok(())
    }

    #[test]
    fn authorizer_deny_unsupported_principal() -> Result<(), KernelError> {
        let mut auth = StaticAuthorizer::empty();
        auth.register(ActionPolicy {
            action: ActionId::new("compute", "AdminAction")?,
            expected_resource_type: ResourceType::new("compute", "server")?,
            accepted_principals: vec![PrincipalKind::Service],
            require_ownership: false,
            required_roles: vec![],
        });

        let user_ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(ResourceType::new("compute", "server")?, None);
        let req = AuthorizationRequest {
            auth_context: &user_ctx,
            action: ActionId::new("compute", "AdminAction")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::UnsupportedPrincipal);
        Ok(())
    }

    #[test]
    fn auth_context_contains_no_raw_tokens_or_secrets() -> Result<(), Box<dyn std::error::Error>> {
        let ctx = test_user_context("usr-1", "proj-1");
        let serialized = serde_json::to_string(&ctx)?;
        assert!(!serialized.contains("token_id"));
        assert!(!serialized.contains("x-auth-token"));
        assert!(!serialized.contains("password"));
        assert!(!serialized.contains("secret"));
        Ok(())
    }

    #[test]
    fn registry_standard_contains_expected_services() -> Result<(), KernelError> {
        let reg =
            KernelRegistry::standard("http://127.0.0.1:18080", Some("http://127.0.0.1:18776"));
        assert!(reg.service_by_id(&ServiceId::new("identity")?).is_some());
        assert!(reg.service_by_id(&ServiceId::new("image")?).is_some());
        assert!(reg.service_by_id(&ServiceId::new("network")?).is_some());
        assert!(reg.service_by_id(&ServiceId::new("compute")?).is_some());
        assert!(reg.service_by_id(&ServiceId::new("placement")?).is_some());
        assert!(reg.service_by_id(&ServiceId::new("cinder")?).is_some());

        let cinder = reg
            .service_by_id(&ServiceId::new("cinder")?)
            .ok_or_else(|| KernelError::InvalidServiceId("missing cinder service".to_owned()))?;
        assert_eq!(cinder.ownership, ServiceOwnership::ExternalHosted);

        let compute = reg
            .service_by_id(&ServiceId::new("compute")?)
            .ok_or_else(|| KernelError::InvalidServiceId("missing compute service".to_owned()))?;
        assert_eq!(compute.ownership, ServiceOwnership::O3kImplemented);

        Ok(())
    }

    #[test]
    fn registry_keystone_catalog_projection() -> Result<(), KernelError> {
        let reg =
            KernelRegistry::standard("http://127.0.0.1:18080", Some("http://127.0.0.1:18776"));
        let catalog = reg.project_keystone_catalog("project-abc");

        assert_eq!(catalog.len(), 6);
        let service_types: Vec<&str> = catalog.iter().map(|s| s.service_type.as_str()).collect();
        assert_eq!(
            service_types,
            vec![
                "compute",
                "identity",
                "image",
                "network",
                "placement",
                "volumev3"
            ]
        );

        let compute_entry = catalog
            .iter()
            .find(|s| s.service_type == "compute")
            .ok_or_else(|| {
                KernelError::InvalidServiceId("missing compute in catalog".to_owned())
            })?;
        let compute_pub = compute_entry
            .endpoints
            .iter()
            .find(|e| e.interface == "public")
            .ok_or_else(|| {
                KernelError::InvalidServiceId("missing public compute endpoint".to_owned())
            })?;
        assert_eq!(compute_pub.url, "http://127.0.0.1:18080/v2.1/project-abc");

        Ok(())
    }

    #[test]
    fn audit_event_lifecycle_and_sink() -> Result<(), Box<dyn std::error::Error>> {
        let sink = MemoryAuditSink::new();
        let ctx = test_user_context("usr-1", "proj-1");

        let event = AuditEvent::from_auth(
            &ctx,
            ServiceNamespace::new("compute")?,
            ActionId::new("compute", "CreateServer")?,
            AuditOutcome::Succeeded,
        )
        .with_resource(
            ResourceType::new("compute", "server")?,
            Some(ResourceId::new("srv-uuid-1")?),
            Some(ctx.effective_scope().clone()),
        )
        .with_decision(AuthorizationDecision::Allow)
        .with_operation(uuid::Uuid::now_v7());

        sink.record(&event);

        let recorded = sink.events();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].action.to_string(), "compute:CreateServer");
        assert_eq!(recorded[0].outcome, AuditOutcome::Succeeded);
        assert_eq!(recorded[0].request_id, "req-67890");
        assert_eq!(recorded[0].audit_id, "audit-12345");
        assert_eq!(recorded[0].principal_id.to_string(), "usr-1");

        // Verify secret redaction: serialization must never contain raw passwords/keys/tokens
        let json = serde_json::to_string(&recorded[0])?;
        assert!(!json.contains("password"));
        assert!(!json.contains("token"));
        assert!(!json.contains("secret"));
        assert!(!json.contains("chap"));

        // Service principal test
        let svc_ctx = test_service_context("cinder-svc", "proj-1");
        assert_eq!(svc_ctx.principal().kind(), PrincipalKind::Service);
        assert_eq!(
            svc_ctx.service_principal().map(|s| s.name()),
            Some("cinder")
        );

        Ok(())
    }
}
