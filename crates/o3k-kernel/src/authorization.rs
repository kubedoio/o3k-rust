use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::{
    action::ActionId,
    auth_context::AuthContext,
    principal::PrincipalKind,
    resource::{ResourceTarget, ResourceType},
};

/// Authorization request presented to the Cloud Kernel authorizer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationRequest<'a> {
    pub auth_context: &'a AuthContext,
    pub action: ActionId,
    pub resource_target: ResourceTarget,
}

/// Stable reason for authorization decisions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionReason {
    Allowed,
    UnknownAction,
    UnknownResourceType,
    ScopeMismatch,
    MissingOwnership,
    UnsupportedPrincipal,
    UnauthorizedRole,
    ExpiredContext,
}

/// The result of evaluating an authorization request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum AuthorizationDecision {
    Allow,
    Deny { reason: DecisionReason },
}

impl AuthorizationDecision {
    /// Helper to check if the decision is `Allow`.
    #[must_use]
    pub fn is_allowed(&self) -> bool {
        matches!(self, Self::Allow)
    }

    /// Returns the decision reason.
    #[must_use]
    pub fn reason(&self) -> &DecisionReason {
        match self {
            Self::Allow => &DecisionReason::Allowed,
            Self::Deny { reason } => reason,
        }
    }
}

/// Service-neutral authorization port.
pub trait Authorizer: Send + Sync {
    /// Evaluates an authorization request and returns a decision.
    fn authorize(&self, request: &AuthorizationRequest<'_>) -> AuthorizationDecision;
}

/// Static policy definition for an action in the authorization inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActionPolicy {
    pub action: ActionId,
    pub expected_resource_type: ResourceType,
    pub accepted_principals: Vec<PrincipalKind>,
    pub require_ownership: bool,
    pub required_roles: Vec<String>,
}

/// Default Cloud Kernel static authorizer implementing fail-closed policy evaluation.
#[derive(Debug, Clone)]
pub struct StaticAuthorizer {
    policies: HashMap<ActionId, ActionPolicy>,
}

impl Default for StaticAuthorizer {
    fn default() -> Self {
        Self::standard()
    }
}

impl StaticAuthorizer {
    /// Creates an authorizer with the standard built-in TestLab action inventory.
    #[must_use]
    pub fn standard() -> Self {
        let mut authorizer = Self {
            policies: HashMap::new(),
        };
        authorizer.register_standard_actions();
        authorizer
    }

    /// Creates an empty static authorizer (will deny all actions until policies are added).
    #[must_use]
    pub fn empty() -> Self {
        Self {
            policies: HashMap::new(),
        }
    }

    /// Registers a policy for an action.
    pub fn register(&mut self, policy: ActionPolicy) {
        self.policies.insert(policy.action.clone(), policy);
    }

    fn register_standard_actions(&mut self) {
        // Standard action inventory helper
        let mut reg = |ns: &str, act: &str, res_ns: &str, res_name: &str, require_owner: bool| {
            if let (Ok(action), Ok(expected_resource_type)) =
                (ActionId::new(ns, act), ResourceType::new(res_ns, res_name))
            {
                self.policies.insert(
                    action.clone(),
                    ActionPolicy {
                        action,
                        expected_resource_type,
                        accepted_principals: vec![PrincipalKind::User, PrincipalKind::Service],
                        require_ownership: require_owner,
                        required_roles: vec![],
                    },
                );
            }
        };

        // Identity
        reg("identity", "IssueToken", "identity", "token", false);
        reg("identity", "ValidateToken", "identity", "token", false);
        reg("identity", "RevokeToken", "identity", "token", false);

        // Image
        reg("image", "ListImages", "image", "image", true);
        reg("image", "CreateImage", "image", "image", true);
        reg("image", "ReadImage", "image", "image", true);
        reg("image", "DeleteImage", "image", "image", true);
        reg("image", "UploadImage", "image", "image", true);
        reg("image", "DownloadImage", "image", "image", true);

        // Network
        reg("network", "ListNetworks", "network", "network", true);
        reg("network", "CreateNetwork", "network", "network", true);
        reg("network", "ReadNetwork", "network", "network", true);
        reg("network", "DeleteNetwork", "network", "network", true);
        reg("network", "ListSubnets", "network", "subnet", true);
        reg("network", "CreateSubnet", "network", "subnet", true);
        reg("network", "ReadSubnet", "network", "subnet", true);
        reg("network", "DeleteSubnet", "network", "subnet", true);
        reg("network", "ListPorts", "network", "port", true);
        reg("network", "CreatePort", "network", "port", true);
        reg("network", "ReadPort", "network", "port", true);
        reg("network", "DeletePort", "network", "port", true);
        reg("network", "ListExtensions", "network", "extension", false);

        // Compute
        reg("compute", "ListFlavors", "compute", "flavor", true);
        reg("compute", "CreateFlavor", "compute", "flavor", true);
        reg("compute", "ReadFlavor", "compute", "flavor", true);
        reg("compute", "DeleteFlavor", "compute", "flavor", true);
        reg("compute", "ListKeypairs", "compute", "keypair", true);
        reg("compute", "ImportKeypair", "compute", "keypair", true);
        reg("compute", "ReadKeypair", "compute", "keypair", true);
        reg("compute", "DeleteKeypair", "compute", "keypair", true);
        reg("compute", "ListServers", "compute", "server", true);
        reg("compute", "CreateServer", "compute", "server", true);
        reg("compute", "ReadServer", "compute", "server", true);
        reg("compute", "DeleteServer", "compute", "server", true);
        reg("compute", "StopServer", "compute", "server", true);
        reg("compute", "StartServer", "compute", "server", true);
        reg("compute", "RebootServer", "compute", "server", true);
        reg("compute", "ReadConsole", "compute", "server", true);

        // Volume
        reg(
            "volume",
            "ListVolumeAttachments",
            "volume",
            "volume_attachment",
            true,
        );
        reg(
            "volume",
            "AttachVolume",
            "volume",
            "volume_attachment",
            true,
        );
        reg(
            "volume",
            "ReadVolumeAttachment",
            "volume",
            "volume_attachment",
            true,
        );
        reg(
            "volume",
            "DetachVolume",
            "volume",
            "volume_attachment",
            true,
        );
    }
}

impl Authorizer for StaticAuthorizer {
    fn authorize(&self, request: &AuthorizationRequest<'_>) -> AuthorizationDecision {
        // 1. Look up policy for the requested action
        let Some(policy) = self.policies.get(&request.action) else {
            return AuthorizationDecision::Deny {
                reason: DecisionReason::UnknownAction,
            };
        };

        // 2. Validate resource type matches policy
        if request.resource_target.resource_type() != &policy.expected_resource_type {
            return AuthorizationDecision::Deny {
                reason: DecisionReason::UnknownResourceType,
            };
        }

        // 3. Validate principal kind is supported
        let principal_kind = request.auth_context.principal().kind();
        if !policy.accepted_principals.contains(&principal_kind) {
            return AuthorizationDecision::Deny {
                reason: DecisionReason::UnsupportedPrincipal,
            };
        }

        // 4. Validate ownership if required
        if policy.require_ownership {
            let caller_scope_id = request.auth_context.effective_scope().id();
            match request.resource_target.owner_scope() {
                Some(target_scope) => {
                    if target_scope != caller_scope_id {
                        return AuthorizationDecision::Deny {
                            reason: DecisionReason::ScopeMismatch,
                        };
                    }
                }
                None => {
                    return AuthorizationDecision::Deny {
                        reason: DecisionReason::MissingOwnership,
                    };
                }
            }
        }

        // 5. Validate required roles if any
        for required_role in &policy.required_roles {
            if !request.auth_context.has_role(required_role) {
                return AuthorizationDecision::Deny {
                    reason: DecisionReason::UnauthorizedRole,
                };
            }
        }

        AuthorizationDecision::Allow
    }
}
