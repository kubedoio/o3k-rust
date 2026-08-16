//! O3K Cloud Kernel: the foundational IAM, authorization, resource ownership,
//! and platform contracts shared across all first-class O3K cloud services.
//!
//! Architectural invariants (ADR-0165, ADR-0166, SPEC-0020):
//! - Inward-facing core crate: must never depend on API, store, identity, provider,
//!   or framework crates.
//! - Canonical IAM & AuthContext: single authoritative `AuthContext` consumed by
//!   all application services.
//! - Service-neutral authorization: `Principal × Action × Resource × Context -> Allow/Deny`.
//! - Default-deny fail-closed policy model.

pub mod action;
pub mod auth_context;
pub mod authorization;
pub mod error;
pub mod principal;
pub mod resource;
pub mod scope;

pub use action::ActionId;
pub use auth_context::AuthContext;
pub use authorization::{
    ActionPolicy, AuthorizationDecision, AuthorizationRequest, Authorizer, DecisionReason,
    StaticAuthorizer,
};
pub use error::KernelError;
pub use principal::{Principal, PrincipalId, PrincipalKind, ServicePrincipal, UserPrincipal};
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
            Principal::Service(service),
            scope,
            vec!["service".to_string(), "admin".to_string()],
            1700000000,
            1700003600,
            "audit-srv-12345",
            "req-srv-67890",
            None,
        )
    }

    #[test]
    fn principal_id_validation() -> Result<(), KernelError> {
        assert!(PrincipalId::new("").is_err());
        assert!(PrincipalId::new("   ").is_err());
        let pid = PrincipalId::new("usr-123")?;
        assert_eq!(pid.as_str(), "usr-123");
        Ok(())
    }

    #[test]
    fn user_and_service_principals() {
        let u_ctx = test_user_context("usr-1", "proj-1");
        assert_eq!(u_ctx.principal().kind(), PrincipalKind::User);
        assert_eq!(u_ctx.principal().id().as_str(), "usr-1");
        assert_eq!(u_ctx.principal().name(), "test-user");

        let s_ctx = test_service_context("srv-1", "proj-1");
        assert_eq!(s_ctx.principal().kind(), PrincipalKind::Service);
        assert_eq!(s_ctx.principal().id().as_str(), "srv-1");
        assert_eq!(s_ctx.principal().name(), "cinder");
    }

    #[test]
    fn scope_id_round_trip() -> Result<(), KernelError> {
        assert!(ScopeId::new("").is_err());
        let sid = ScopeId::new("proj-abc")?;
        assert_eq!(sid.as_str(), "proj-abc");
        Ok(())
    }

    #[test]
    fn resource_target_collection_and_instance() -> Result<(), KernelError> {
        let r_type = ResourceType::new("compute", "server")?;
        let s_id = ScopeId::new("proj-1")?;
        let r_id = ResourceId::new("srv-uuid-1")?;

        let col_target = ResourceTarget::collection(r_type.clone(), Some(s_id.clone()));
        assert_eq!(col_target.resource_type(), &r_type);
        assert_eq!(col_target.owner_scope(), Some(&s_id));
        assert_eq!(col_target.resource_id(), None);

        let inst_target =
            ResourceTarget::instance(r_type.clone(), r_id.clone(), Some(s_id.clone()));
        assert_eq!(inst_target.resource_type(), &r_type);
        assert_eq!(inst_target.owner_scope(), Some(&s_id));
        assert_eq!(inst_target.resource_id(), Some(&r_id));
        Ok(())
    }

    #[test]
    fn typed_action_equality_and_parsing() -> Result<(), KernelError> {
        let a1 = ActionId::new("compute", "CreateServer")?;
        let a2 = ActionId::parse("compute:CreateServer")?;
        assert_eq!(a1, a2);
        assert_eq!(a1.namespace(), "compute");
        assert_eq!(a1.action(), "CreateServer");
        assert!(ActionId::parse("invalid").is_err());
        Ok(())
    }

    #[test]
    fn authorizer_allow_matching_scope() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(
            ResourceType::new("compute", "server")?,
            Some(ScopeId::new("proj-1")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "CreateServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::Allowed);
        Ok(())
    }

    #[test]
    fn authorizer_deny_wrong_scope() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(
            ResourceType::new("compute", "server")?,
            Some(ScopeId::new("proj-2")?),
        );
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "CreateServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::ScopeMismatch);
        Ok(())
    }

    #[test]
    fn authorizer_deny_missing_ownership() -> Result<(), KernelError> {
        let auth = StaticAuthorizer::standard();
        let ctx = test_user_context("usr-1", "proj-1");
        let target = ResourceTarget::collection(ResourceType::new("compute", "server")?, None);
        let req = AuthorizationRequest {
            auth_context: &ctx,
            action: ActionId::new("compute", "CreateServer")?,
            resource_target: target,
        };
        let decision = auth.authorize(&req);
        assert!(!decision.is_allowed());
        assert_eq!(decision.reason(), &DecisionReason::MissingOwnership);
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
}
