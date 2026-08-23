//! Safe Rust-first helpers for implementing an external O3K controller.
//! Business handlers receive validated domain requests; transport/session and
//! replay plumbing remains in this crate.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use o3k_kernel::{ControllerFailure, FailureCategory, OperationContext};
use prost::Message;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{collections::HashMap, sync::Arc};
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

pub mod tls {
    use super::SdkError;
    use std::{fs, path::Path};
    use tonic::transport::{Certificate, ClientTlsConfig, Identity, ServerTlsConfig};

    fn read(path: impl AsRef<Path>, label: &'static str) -> Result<Vec<u8>, SdkError> {
        fs::read(path).map_err(|error| SdkError::Invalid(format!("cannot read {label}: {error}")))
    }

    pub fn client(
        ca: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
        server_name: &str,
    ) -> Result<ClientTlsConfig, SdkError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        if server_name.trim().is_empty() {
            return Err(SdkError::Invalid("TLS server name is required".into()));
        }
        Ok(ClientTlsConfig::new()
            .ca_certificate(Certificate::from_pem(read(ca, "CA")?))
            .identity(Identity::from_pem(
                read(certificate, "client certificate")?,
                read(key, "client key")?,
            ))
            .domain_name(server_name))
    }

    pub fn server(
        ca: impl AsRef<Path>,
        certificate: impl AsRef<Path>,
        key: impl AsRef<Path>,
    ) -> Result<ServerTlsConfig, SdkError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        Ok(ServerTlsConfig::new()
            .identity(Identity::from_pem(
                read(certificate, "server certificate")?,
                read(key, "server key")?,
            ))
            .client_ca_root(Certificate::from_pem(read(ca, "client CA")?)))
    }
}

/// Generic service-to-service composition port. Implementations are supplied
/// by the O3K control plane; external services receive only bounded child
/// authority and never provider/store access.
pub mod composition {
    use ed25519_dalek::VerifyingKey;
    use o3k_controller_protocol::composition as wire;
    use o3k_kernel::{
        ActionId, OperationContext, OwnershipScope, RelationshipOwnership, ResourceId,
        ResourceReference, ResourceType,
    };
    use std::collections::HashMap;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};
    use uuid::Uuid;

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChildResourceRequest {
        pub parent: ResourceReference,
        pub parent_operation_id: Uuid,
        pub child_operation_id: Option<Uuid>,
        pub context: OperationContext,
        pub service_principal: String,
        pub delegation: Vec<u8>,
        pub child: Option<ResourceReference>,
        pub action: ActionId,
        pub resource_type: o3k_kernel::ResourceType,
        pub owner_scope: OwnershipScope,
        pub slot: String,
        pub idempotency_key: String,
        pub desired_spec: serde_json::Value,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct ChildResourceReceipt {
        pub resource: ResourceReference,
        pub operation_id: Uuid,
        pub owner_scope: OwnershipScope,
        pub ownership: RelationshipOwnership,
    }

    #[derive(Debug, thiserror::Error)]
    pub enum CompositionError {
        #[error("child request is not authorized")]
        Unauthorized,
        #[error("child operation outcome is unknown")]
        UnknownOutcome,
        #[error("child operation failed: {0}")]
        Failed(String),
    }

    fn scope_to_wire(scope: &OwnershipScope) -> wire::Scope {
        wire::Scope {
            id: scope.id().as_str().to_owned(),
            kind: match scope.kind() {
                o3k_kernel::ScopeKind::Project => "project",
                o3k_kernel::ScopeKind::Domain => "domain",
                o3k_kernel::ScopeKind::System => "system",
            }
            .to_owned(),
        }
    }

    fn resource_to_wire(reference: &ResourceReference) -> wire::ResourceRef {
        wire::ResourceRef {
            namespace: reference.resource_type.namespace().to_owned(),
            r#type: reference.resource_type.name().to_owned(),
            id: reference.resource_id.as_str().to_owned(),
            generation: reference.generation,
        }
    }

    /// Converts the validated domain request into the language-neutral wire
    /// request. Identity/session/delegation fields are populated from the
    /// canonical operation context; no caller-supplied strings are inferred.
    pub fn child_request_to_wire(
        request: &ChildResourceRequest,
        lifecycle: &str,
    ) -> Result<wire::ChildRequest, CompositionError> {
        if request.context.operation_id != request.parent_operation_id {
            return Err(CompositionError::Failed(
                "parent operation does not match request context".into(),
            ));
        }
        if request.context.owner_scope != request.owner_scope {
            return Err(CompositionError::Failed(
                "owner scope does not match request context".into(),
            ));
        }
        let parent = wire::ParentContext {
            request_id: request.context.request_id.to_string(),
            operation_id: request.context.operation_id.to_string(),
            service_id: request.context.service_id.clone(),
            service_principal: request.service_principal.clone(),
            parent_action: request.context.action.to_string(),
            parent: Some(resource_to_wire(&request.parent)),
            parent_generation: request.parent.generation,
            owner_scope: Some(scope_to_wire(&request.context.owner_scope)),
            session_id: request.context.session_id.to_string(),
            session_generation: request.context.session_generation,
            slot: request.slot.clone(),
            replay_identity: request.context.replay_identity.clone(),
            audit_correlation: request.context.audit_correlation.clone(),
            delegation: request.delegation.clone(),
        };
        Ok(wire::ChildRequest {
            parent: Some(parent),
            lifecycle: lifecycle.to_owned(),
            resource_type: request.resource_type.to_string(),
            desired_spec: serde_json::to_vec(&request.desired_spec)
                .map_err(|_| CompositionError::Failed("invalid child spec".into()))?,
            requested_action: request.action.to_string(),
            child: request.child.as_ref().map(resource_to_wire),
        })
    }

    /// Strictly validates a wire resource reference before it can enter the
    /// canonical domain model.
    pub fn resource_from_wire(
        value: wire::ResourceRef,
    ) -> Result<ResourceReference, CompositionError> {
        let resource_type = ResourceType::new(&value.namespace, &value.r#type)
            .map_err(|_| CompositionError::Failed("invalid resource type".into()))?;
        let resource_id = ResourceId::new(&value.id)
            .map_err(|_| CompositionError::Failed("invalid resource id".into()))?;
        Ok(ResourceReference {
            resource_type,
            resource_id,
            generation: value.generation,
        })
    }

    fn parse_uuid(value: &str, field: &str) -> Result<Uuid, CompositionError> {
        Uuid::parse_str(value).map_err(|_| CompositionError::Failed(format!("invalid {field}")))
    }

    fn scope_from_wire(value: wire::Scope) -> Result<OwnershipScope, CompositionError> {
        let id = o3k_kernel::ScopeId::new(value.id)
            .map_err(|_| CompositionError::Failed("invalid owner scope id".into()))?;
        match value.kind.as_str() {
            "project" => Ok(OwnershipScope::project(id, None, None)),
            "domain" => Ok(OwnershipScope::new(
                id,
                o3k_kernel::ScopeKind::Domain,
                None,
                None,
            )),
            "system" => Ok(OwnershipScope::new(
                id,
                o3k_kernel::ScopeKind::System,
                None,
                None,
            )),
            _ => Err(CompositionError::Failed("invalid owner scope kind".into())),
        }
    }

    fn child_request_from_wire(
        request: wire::ChildRequest,
    ) -> Result<ChildResourceRequest, CompositionError> {
        let parent = request
            .parent
            .ok_or_else(|| CompositionError::Failed("missing parent context".into()))?;
        let parent_ref = parent
            .parent
            .ok_or_else(|| CompositionError::Failed("missing parent resource".into()))
            .and_then(resource_from_wire)?;
        let owner_scope = parent
            .owner_scope
            .ok_or_else(|| CompositionError::Failed("missing owner scope".into()))
            .and_then(scope_from_wire)?;
        let operation_id = parse_uuid(&parent.operation_id, "operation id")?;
        let request_id = parse_uuid(&parent.request_id, "request id")?;
        let session_id = parse_uuid(&parent.session_id, "session id")?;
        let parent_action = ActionId::parse(&parent.parent_action)
            .map_err(|_| CompositionError::Failed("invalid parent action".into()))?;
        let (namespace, name) = request
            .resource_type
            .split_once(':')
            .ok_or_else(|| CompositionError::Failed("invalid child resource type".into()))?;
        let resource_type = ResourceType::new(namespace, name)
            .map_err(|_| CompositionError::Failed("invalid child resource type".into()))?;
        let desired_spec: serde_json::Value = serde_json::from_slice(&request.desired_spec)
            .map_err(|_| CompositionError::Failed("invalid child desired spec".into()))?;
        let child_action = ActionId::parse(&request.requested_action)
            .map_err(|_| CompositionError::Failed("invalid child action".into()))?;
        let child = request.child.map(resource_from_wire).transpose()?;
        Ok(ChildResourceRequest {
            parent: parent_ref,
            parent_operation_id: operation_id,
            child_operation_id: None,
            context: OperationContext {
                request_id,
                operation_id,
                action: parent_action,
                service_id: parent.service_id.clone(),
                owner_scope: owner_scope.clone(),
                session_id,
                session_generation: parent.session_generation,
                deadline_unix_ms: 0,
                replay_identity: parent.replay_identity.clone(),
                audit_correlation: parent.audit_correlation.clone(),
            },
            service_principal: parent.service_principal,
            delegation: parent.delegation,
            child,
            action: child_action,
            resource_type,
            owner_scope,
            slot: parent.slot,
            idempotency_key: parent.replay_identity,
            desired_spec,
        })
    }

    fn bind_parent_delegation(
        parent: &wire::ParentContext,
        resource: Option<&wire::ResourceRef>,
        expected_recipient: &str,
        keys: &HashMap<String, VerifyingKey>,
    ) -> Result<(), CompositionError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| CompositionError::Unauthorized)?
            .as_millis() as u64;
        let claims = super::verify_wire_delegation(&parent.delegation, keys, now)
            .map_err(|_| CompositionError::Unauthorized)?;
        let request_id = parse_uuid(&parent.request_id, "request id")?;
        let operation_id = parse_uuid(&parent.operation_id, "operation id")?;
        let session_id = parse_uuid(&parent.session_id, "session id")?;
        let scope = parent
            .owner_scope
            .as_ref()
            .ok_or(CompositionError::Unauthorized)?;
        let scope_kind = match scope.kind.as_str() {
            "project" | "domain" | "system" => scope.kind.as_str(),
            _ => return Err(CompositionError::Unauthorized),
        };
        if claims.request_id != request_id
            || claims.operation_id != operation_id
            || claims.session_id != session_id
            || claims.session_generation != parent.session_generation
            || claims.calling_service != parent.service_id
            || claims.recipient_service != expected_recipient
            || claims.action != parent.parent_action
            || claims.owner_scope != format!("{scope_kind}:{}", scope.id)
        {
            return Err(CompositionError::Unauthorized);
        }
        if let Some(resource) = resource
            && (claims.resource_type != format!("{}:{}", resource.namespace, resource.r#type)
                || claims.resource_id != resource.id
                || resource.generation < 0)
        {
            return Err(CompositionError::Unauthorized);
        }
        Ok(())
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub struct RelationshipView {
        pub slot: String,
        pub resource: Option<ResourceReference>,
        pub resource_type: ResourceType,
        pub ownership: RelationshipOwnership,
        pub state: String,
        pub parent_operation_id: Uuid,
        pub child_operation_id: Option<Uuid>,
    }

    #[tonic::async_trait]
    pub trait CompositionHandler: Send + Sync + 'static {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError>;
        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
        async fn list_relationships(
            &self,
            request: ChildResourceRequest,
        ) -> Result<Vec<RelationshipView>, CompositionError>;
    }

    fn composition_status(error: CompositionError) -> tonic::Status {
        match error {
            CompositionError::Unauthorized => {
                tonic::Status::permission_denied("composition request denied")
            }
            CompositionError::UnknownOutcome => {
                tonic::Status::unknown("composition outcome is unknown")
            }
            CompositionError::Failed(detail) => tonic::Status::invalid_argument(detail),
        }
    }

    pub(crate) fn composition_transport_error(
        error: tonic::Status,
        mutation: bool,
    ) -> CompositionError {
        match error.code() {
            tonic::Code::Unauthenticated | tonic::Code::PermissionDenied => {
                CompositionError::Unauthorized
            }
            tonic::Code::Unknown | tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
                if mutation =>
            {
                CompositionError::UnknownOutcome
            }
            _ => CompositionError::Failed(error.message().to_owned()),
        }
    }

    #[allow(clippy::result_large_err)]
    fn validate_binding(
        parent: &wire::ParentContext,
        expected_service_id: &str,
        expected_service_principal: &str,
    ) -> Result<(), tonic::Status> {
        if parent.service_id != expected_service_id
            || parent.service_principal != expected_service_principal
        {
            return Err(tonic::Status::permission_denied(
                "composition service identity mismatch",
            ));
        }
        Ok(())
    }

    fn receipt_response(
        parent: &wire::ParentContext,
        receipt: ChildResourceReceipt,
        state: &str,
        observed_status: Vec<u8>,
    ) -> wire::ChildResponse {
        wire::ChildResponse {
            resource: Some(resource_to_wire(&receipt.resource)),
            operation_id: receipt.operation_id.to_string(),
            state: state.to_owned(),
            observed_status,
            diagnostic: String::new(),
            request_id: parent.request_id.clone(),
            service_id: parent.service_id.clone(),
            session_id: parent.session_id.clone(),
            session_generation: parent.session_generation,
            parent_operation_id: parent.operation_id.clone(),
            slot: parent.slot.clone(),
            ownership: match receipt.ownership {
                RelationshipOwnership::Exclusive => "exclusive",
                RelationshipOwnership::Referenced => "referenced",
            }
            .to_owned(),
        }
    }

    pub struct CompositionServiceAdapter<H> {
        handler: Arc<H>,
        expected_service_id: String,
        expected_service_principal: String,
        expected_recipient_service: String,
        delegation_keys: Arc<HashMap<String, VerifyingKey>>,
    }

    impl<H> CompositionServiceAdapter<H> {
        pub fn new(
            handler: Arc<H>,
            expected_service_id: impl Into<String>,
            expected_service_principal: impl Into<String>,
        ) -> Self {
            Self {
                handler,
                expected_service_id: expected_service_id.into(),
                expected_service_principal: expected_service_principal.into(),
                expected_recipient_service: "o3k-composition".into(),
                delegation_keys: Arc::new(HashMap::new()),
            }
        }

        #[must_use]
        pub fn with_delegation_keys(
            mut self,
            expected_recipient_service: impl Into<String>,
            keys: HashMap<String, VerifyingKey>,
        ) -> Self {
            self.expected_recipient_service = expected_recipient_service.into();
            self.delegation_keys = Arc::new(keys);
            self
        }

        pub fn into_server(
            self,
        ) -> wire::composition_service_server::CompositionServiceServer<Self> {
            wire::composition_service_server::CompositionServiceServer::new(self)
        }
    }

    #[tonic::async_trait]
    impl<H: CompositionHandler> wire::composition_service_server::CompositionService
        for CompositionServiceAdapter<H>
    {
        async fn create_child(
            &self,
            request: tonic::Request<wire::ChildRequest>,
        ) -> Result<tonic::Response<wire::ChildResponse>, tonic::Status> {
            let wire_request = request.into_inner();
            let parent = wire_request
                .parent
                .clone()
                .ok_or_else(|| tonic::Status::invalid_argument("missing parent context"))?;
            validate_binding(
                &parent,
                &self.expected_service_id,
                &self.expected_service_principal,
            )?;
            bind_parent_delegation(
                &parent,
                parent.parent.as_ref(),
                &self.expected_recipient_service,
                &self.delegation_keys,
            )
            .map_err(composition_status)?;
            let domain = child_request_from_wire(wire_request).map_err(composition_status)?;
            let receipt = self
                .handler
                .create_child(domain)
                .await
                .map_err(composition_status)?;
            Ok(tonic::Response::new(receipt_response(
                &parent,
                receipt,
                "accepted",
                Vec::new(),
            )))
        }

        async fn observe_child(
            &self,
            request: tonic::Request<wire::ObserveRequest>,
        ) -> Result<tonic::Response<wire::ChildResponse>, tonic::Status> {
            let request = request.into_inner();
            let parent = request
                .parent
                .clone()
                .ok_or_else(|| tonic::Status::invalid_argument("missing parent context"))?;
            validate_binding(
                &parent,
                &self.expected_service_id,
                &self.expected_service_principal,
            )?;
            bind_parent_delegation(
                &parent,
                parent.parent.as_ref(),
                &self.expected_recipient_service,
                &self.delegation_keys,
            )
            .map_err(composition_status)?;
            let resource = request
                .resource
                .clone()
                .ok_or_else(|| tonic::Status::invalid_argument("missing child resource"))?;
            let resource = resource_from_wire(resource).map_err(composition_status)?;
            let mut child_request = child_request_from_wire(wire::ChildRequest {
                parent: Some(parent.clone()),
                lifecycle: "observe".into(),
                resource_type: resource.resource_type.to_string(),
                desired_spec: b"null".to_vec(),
                requested_action: parent.parent_action.clone(),
                child: Some(resource_to_wire(&resource)),
            })
            .map_err(composition_status)?;
            child_request.child = Some(resource.clone());
            child_request.child_operation_id = Some(
                parse_uuid(&request.child_operation_id, "child operation id")
                    .map_err(composition_status)?,
            );
            let status = self
                .handler
                .observe_child(child_request)
                .await
                .map_err(composition_status)?;
            Ok(tonic::Response::new(wire::ChildResponse {
                resource: Some(resource_to_wire(&resource)),
                operation_id: request.child_operation_id,
                state: "observed".into(),
                observed_status: serde_json::to_vec(&status)
                    .map_err(|_| tonic::Status::internal("cannot encode observation"))?,
                diagnostic: String::new(),
                request_id: parent.request_id,
                service_id: parent.service_id,
                session_id: parent.session_id,
                session_generation: parent.session_generation,
                parent_operation_id: parent.operation_id,
                slot: parent.slot,
                ownership: String::new(),
            }))
        }

        async fn delete_child(
            &self,
            request: tonic::Request<wire::DeleteRequest>,
        ) -> Result<tonic::Response<wire::ChildResponse>, tonic::Status> {
            let child = request
                .into_inner()
                .child
                .ok_or_else(|| tonic::Status::invalid_argument("missing child request"))?;
            let parent = child
                .parent
                .clone()
                .ok_or_else(|| tonic::Status::invalid_argument("missing parent context"))?;
            validate_binding(
                &parent,
                &self.expected_service_id,
                &self.expected_service_principal,
            )?;
            bind_parent_delegation(
                &parent,
                child
                    .parent
                    .as_ref()
                    .and_then(|value| value.parent.as_ref()),
                &self.expected_recipient_service,
                &self.delegation_keys,
            )
            .map_err(composition_status)?;
            let domain = child_request_from_wire(child).map_err(composition_status)?;
            let receipt = self
                .handler
                .delete_child(domain)
                .await
                .map_err(composition_status)?;
            Ok(tonic::Response::new(receipt_response(
                &parent,
                receipt,
                "accepted",
                Vec::new(),
            )))
        }

        async fn list_relationships(
            &self,
            request: tonic::Request<wire::RelationshipRequest>,
        ) -> Result<tonic::Response<wire::RelationshipResponse>, tonic::Status> {
            let parent = request
                .into_inner()
                .parent
                .ok_or_else(|| tonic::Status::invalid_argument("missing parent context"))?;
            validate_binding(
                &parent,
                &self.expected_service_id,
                &self.expected_service_principal,
            )?;
            bind_parent_delegation(
                &parent,
                parent.parent.as_ref(),
                &self.expected_recipient_service,
                &self.delegation_keys,
            )
            .map_err(composition_status)?;
            let domain = child_request_from_wire(wire::ChildRequest {
                parent: Some(parent),
                lifecycle: "list".into(),
                resource_type: "relationship:record".into(),
                desired_spec: b"null".to_vec(),
                requested_action: "relationship:List".into(),
                child: None,
            })
            .map_err(composition_status)?;
            let relationships = self
                .handler
                .list_relationships(domain)
                .await
                .map_err(composition_status)?;
            Ok(tonic::Response::new(wire::RelationshipResponse {
                relationships: relationships
                    .into_iter()
                    .map(|relationship| wire::Relationship {
                        slot: relationship.slot,
                        resource_type: relationship.resource_type.to_string(),
                        resource: relationship.resource.map(|value| resource_to_wire(&value)),
                        ownership: match relationship.ownership {
                            RelationshipOwnership::Exclusive => "exclusive".into(),
                            RelationshipOwnership::Referenced => "referenced".into(),
                        },
                        state: relationship.state,
                        parent_operation_id: relationship.parent_operation_id.to_string(),
                        child_operation_id: relationship
                            .child_operation_id
                            .map(|value| value.to_string())
                            .unwrap_or_default(),
                    })
                    .collect(),
            }))
        }
    }

    fn response_to_receipt(
        request: &ChildResourceRequest,
        response: wire::ChildResponse,
    ) -> Result<ChildResourceReceipt, CompositionError> {
        let expected_request = request.context.request_id.to_string();
        if response.request_id != expected_request
            || response.service_id != request.context.service_id
            || response.session_id != request.context.session_id.to_string()
            || response.session_generation != request.context.session_generation
            || response.parent_operation_id != request.parent_operation_id.to_string()
            || response.slot != request.slot
        {
            return Err(CompositionError::Failed(
                "composition response correlation mismatch".into(),
            ));
        }
        let resource = response
            .resource
            .ok_or_else(|| CompositionError::Failed("composition response omitted resource".into()))
            .and_then(resource_from_wire)?;
        if resource.resource_type != request.resource_type {
            return Err(CompositionError::Failed(
                "composition response resource mismatch".into(),
            ));
        }
        let operation_id = parse_uuid(&response.operation_id, "child operation id")?;
        if let Some(expected) = request.child_operation_id
            && operation_id != expected
        {
            return Err(CompositionError::Failed(
                "composition child operation mismatch".into(),
            ));
        }
        let owner_scope = request.owner_scope.clone();
        let ownership = match response.ownership.as_str() {
            "exclusive" => RelationshipOwnership::Exclusive,
            "referenced" => RelationshipOwnership::Referenced,
            _ => {
                return Err(CompositionError::Failed(
                    "invalid relationship ownership".into(),
                ));
            }
        };
        Ok(ChildResourceReceipt {
            resource,
            operation_id,
            owner_scope,
            ownership,
        })
    }

    /// Real generic controller-to-O3K composition client. It contains only
    /// the versioned composition transport and typed conversions; the O3K
    /// server remains responsible for authorization, descriptor resolution,
    /// delegation verification, and durable relationship writes.
    pub struct GrpcCompositionClient {
        client: tokio::sync::Mutex<
            wire::composition_service_client::CompositionServiceClient<tonic::transport::Channel>,
        >,
    }

    impl GrpcCompositionClient {
        pub async fn connect(
            endpoint: &str,
            tls: tonic::transport::ClientTlsConfig,
        ) -> Result<Self, CompositionError> {
            let channel = tonic::transport::Endpoint::from_shared(endpoint.to_owned())
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .tls_config(tls)
                .map_err(|error| CompositionError::Failed(error.to_string()))?
                .connect()
                .await
                .map_err(|error| CompositionError::Failed(error.to_string()))?;
            Ok(Self {
                client: tokio::sync::Mutex::new(
                    wire::composition_service_client::CompositionServiceClient::new(channel),
                ),
            })
        }
    }

    #[tonic::async_trait]
    pub trait ServiceCompositionClient: Send + Sync {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError>;
        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError>;
        async fn delete_child(&self, request: ChildResourceRequest)
        -> Result<(), CompositionError>;
        async fn list_relationships(
            &self,
            request: ChildResourceRequest,
        ) -> Result<Vec<RelationshipView>, CompositionError>;
    }

    #[tonic::async_trait]
    impl ServiceCompositionClient for GrpcCompositionClient {
        async fn create_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<ChildResourceReceipt, CompositionError> {
            let wire_request = child_request_to_wire(&request, "create")?;
            let response = self
                .client
                .lock()
                .await
                .create_child(wire_request)
                .await
                .map_err(|error| composition_transport_error(error, true))?
                .into_inner();
            response_to_receipt(&request, response)
        }

        async fn observe_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<serde_json::Value, CompositionError> {
            let child = request
                .child
                .clone()
                .ok_or_else(|| CompositionError::Failed("missing child reference".into()))?;
            let mut client = self.client.lock().await;
            let response = client
                .observe_child(wire::ObserveRequest {
                    parent: Some(
                        child_request_to_wire(&request, "observe")?
                            .parent
                            .ok_or_else(|| CompositionError::Failed("missing parent".into()))?,
                    ),
                    resource: Some(resource_to_wire(&child)),
                    child_operation_id: request
                        .child_operation_id
                        .ok_or_else(|| CompositionError::Failed("missing child operation".into()))?
                        .to_string(),
                })
                .await
                .map_err(|error| composition_transport_error(error, false))?
                .into_inner();
            if response.request_id != request.context.request_id.to_string()
                || response.session_id != request.context.session_id.to_string()
                || response.session_generation != request.context.session_generation
                || response.parent_operation_id != request.parent_operation_id.to_string()
                || response.slot != request.slot
            {
                return Err(CompositionError::Failed(
                    "composition observation correlation mismatch".into(),
                ));
            }
            serde_json::from_slice(&response.observed_status)
                .map_err(|_| CompositionError::Failed("invalid child observation".into()))
        }

        async fn delete_child(
            &self,
            request: ChildResourceRequest,
        ) -> Result<(), CompositionError> {
            let wire_request = child_request_to_wire(&request, "delete")?;
            let response = self
                .client
                .lock()
                .await
                .delete_child(wire::DeleteRequest {
                    child: Some(wire_request),
                })
                .await
                .map_err(|error| composition_transport_error(error, true))?
                .into_inner();
            let _ = response_to_receipt(&request, response)?;
            Ok(())
        }

        async fn list_relationships(
            &self,
            request: ChildResourceRequest,
        ) -> Result<Vec<RelationshipView>, CompositionError> {
            let parent = child_request_to_wire(&request, "list")?
                .parent
                .ok_or_else(|| CompositionError::Failed("missing parent".into()))?;
            let response = self
                .client
                .lock()
                .await
                .list_relationships(wire::RelationshipRequest {
                    parent: Some(parent),
                })
                .await
                .map_err(|error| composition_transport_error(error, false))?
                .into_inner();
            response
                .relationships
                .into_iter()
                .map(|relationship| {
                    let (namespace, name) =
                        relationship.resource_type.split_once(':').ok_or_else(|| {
                            CompositionError::Failed("invalid relationship resource type".into())
                        })?;
                    let resource_type = ResourceType::new(namespace, name).map_err(|_| {
                        CompositionError::Failed("invalid relationship resource type".into())
                    })?;
                    let resource = relationship.resource.map(resource_from_wire).transpose()?;
                    let parent_operation_id = Uuid::parse_str(&relationship.parent_operation_id)
                        .map_err(|_| {
                            CompositionError::Failed("invalid parent operation id".into())
                        })?;
                    let child_operation_id =
                        if relationship.child_operation_id.is_empty() {
                            None
                        } else {
                            Some(Uuid::parse_str(&relationship.child_operation_id).map_err(
                                |_| CompositionError::Failed("invalid child operation id".into()),
                            )?)
                        };
                    let ownership = match relationship.ownership.as_str() {
                        "exclusive" => RelationshipOwnership::Exclusive,
                        "referenced" => RelationshipOwnership::Referenced,
                        _ => {
                            return Err(CompositionError::Failed(
                                "invalid relationship ownership".into(),
                            ));
                        }
                    };
                    Ok(RelationshipView {
                        slot: relationship.slot,
                        resource,
                        resource_type,
                        ownership,
                        state: relationship.state,
                        parent_operation_id,
                        child_operation_id,
                    })
                })
                .collect()
        }
    }
}

use o3k_controller_protocol::proto::{
    self,
    controller_service_server::{ControllerService, ControllerServiceServer},
};
use tonic::transport::{Channel, Endpoint};

#[tonic::async_trait]
pub trait ControllerHandler: Send + Sync + 'static {
    async fn health(
        &self,
        request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, tonic::Status>;
    async fn capabilities(
        &self,
        request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, tonic::Status>;
    async fn reconcile(
        &self,
        request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, tonic::Status>;
    async fn observe(
        &self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, tonic::Status>;
    async fn delete(
        &self,
        request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, tonic::Status>;
}

pub struct ServiceControllerServer<H> {
    handler: Arc<H>,
    service_id: String,
    namespace: String,
    manifest_digest: String,
    generation: u64,
    fence: SessionFence,
    replay: ReplayLedger,
    delegation_keys: Arc<HashMap<String, VerifyingKey>>,
    expected_service_principal: Option<String>,
    expected_delegation_recipient: String,
}

#[derive(Clone)]
pub struct ExternalControllerClient {
    client: proto::controller_service_client::ControllerServiceClient<Channel>,
}

/// Canonical kernel adapter for an authenticated external controller. Wire
/// types remain private to this transport boundary; callers use the kernel's
/// `Controller` trait and typed outcomes.
pub struct GrpcControllerAdapter {
    client: Mutex<ExternalControllerClient>,
    service_id: String,
    session: o3k_kernel::ControllerSession,
    delegation_key_id: Option<String>,
    delegation_signing_key: Option<SigningKey>,
}

impl GrpcControllerAdapter {
    #[must_use]
    pub fn session(&self) -> &o3k_kernel::ControllerSession {
        &self.session
    }

    fn health_deadline() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_millis() as u64 + 60_000)
            .unwrap_or(60_000)
    }
    pub async fn connect(
        endpoint: &str,
        tls: tonic::transport::ClientTlsConfig,
        service_id: impl Into<String>,
        namespace: impl Into<String>,
        principal: o3k_kernel::ServicePrincipal,
        manifest_digest: impl Into<String>,
        manifest_generation: u64,
    ) -> Result<Self, SdkError> {
        let service_id = service_id.into();
        let namespace = namespace.into();
        let manifest_digest = manifest_digest.into();
        let mut client = ExternalControllerClient::connect(endpoint, tls).await?;
        let registration = client
            .register(proto::RegisterRequest {
                service_id: service_id.clone(),
                namespace: namespace.clone(),
                service_principal_id: principal.name().to_owned(),
                manifest_digest: manifest_digest.clone(),
                manifest_generation,
                supported_versions: vec![proto::Version { major: 1, minor: 0 }],
                capabilities: Vec::new(),
            })
            .await?;
        let version = registration
            .negotiated_version
            .ok_or_else(|| SdkError::Invalid("controller did not negotiate a version".into()))?;
        if version.major != 1 || version.minor != 0 {
            return Err(SdkError::Invalid(
                "unsupported negotiated controller version".into(),
            ));
        }
        let session_id = Uuid::parse_str(&registration.session_id)
            .map_err(|_| SdkError::Invalid("controller returned invalid session id".into()))?;
        Ok(Self {
            client: Mutex::new(client),
            service_id: service_id.clone(),
            session: o3k_kernel::ControllerSession {
                service_id,
                namespace,
                service_principal: principal,
                session_id,
                session_generation: registration.session_generation,
                protocol_version: o3k_kernel::ProtocolVersion::V1,
                manifest_digest,
                manifest_generation,
                started_at: String::new(),
            },
            delegation_key_id: None,
            delegation_signing_key: None,
        })
    }

    #[must_use]
    pub fn with_delegation_signer(mut self, key_id: impl Into<String>, key: SigningKey) -> Self {
        self.delegation_key_id = Some(key_id.into());
        self.delegation_signing_key = Some(key);
        self
    }

    pub fn issue_parent_delegation(
        &self,
        context: &OperationContext,
        original_actor: impl Into<String>,
        resource: &o3k_kernel::ResourceReference,
    ) -> Result<o3k_kernel::DelegationContext, SdkError> {
        let key_id = self
            .delegation_key_id
            .clone()
            .ok_or_else(|| SdkError::Invalid("delegation signer is not configured".into()))?;
        let key = self
            .delegation_signing_key
            .as_ref()
            .ok_or_else(|| SdkError::Invalid("delegation signer is not configured".into()))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| SdkError::Invalid("clock error".into()))?
            .as_millis() as u64;
        let claims = DelegationClaims {
            version: 1,
            credential_id: Uuid::new_v4(),
            issuer: "o3k-control-plane".into(),
            key_id: key_id.clone(),
            original_actor: original_actor.into(),
            owner_scope: context.owner_scope.to_string(),
            calling_service: self.service_id.clone(),
            recipient_service: "o3k-composition".into(),
            action: context.action.to_string(),
            resource_type: resource.resource_type.to_string(),
            resource_id: resource.resource_id.as_str().to_owned(),
            request_id: context.request_id,
            operation_id: context.operation_id,
            session_id: context.session_id,
            session_generation: context.session_generation,
            issued_at_unix_ms: now,
            expires_at_unix_ms: context.deadline_unix_ms,
        };
        let signed = SignedDelegation::sign(claims.clone(), key)?;
        Ok(o3k_kernel::DelegationContext {
            credential_id: claims.credential_id,
            original_actor: claims.original_actor,
            original_scope: context.owner_scope.clone(),
            calling_service: o3k_kernel::ServicePrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked(self.service_id.clone()),
                self.service_id.clone(),
                self.session.namespace.clone(),
            ),
            recipient_service: o3k_kernel::ServicePrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("o3k-composition"),
                "o3k-composition",
                "o3k",
            ),
            parent_action: context.action.clone(),
            delegated_action: context.action.clone(),
            resource: resource.clone(),
            operation_id: context.operation_id,
            request_id: context.request_id,
            audit_correlation: context.audit_correlation.clone(),
            session_id: context.session_id,
            session_generation: context.session_generation,
            issued_at_unix_ms: claims.issued_at_unix_ms,
            expires_at_unix_ms: claims.expires_at_unix_ms,
            key_id,
            signature: signed.signature,
        })
    }

    fn context(&self, context: &OperationContext) -> proto::Context {
        proto::Context {
            request_id: context.request_id.to_string(),
            operation_id: context.operation_id.to_string(),
            action: context.action.to_string(),
            service_id: self.service_id.clone(),
            owner_scope: Some(proto::Scope {
                id: context.owner_scope.id().as_str().to_owned(),
                kind: match context.owner_scope.kind() {
                    o3k_kernel::ScopeKind::Project => proto::scope::Kind::Project as i32,
                    o3k_kernel::ScopeKind::Domain => proto::scope::Kind::Domain as i32,
                    o3k_kernel::ScopeKind::System => proto::scope::Kind::System as i32,
                },
                name: context.owner_scope.name().unwrap_or_default().to_owned(),
                domain_id: context
                    .owner_scope
                    .domain_id()
                    .unwrap_or_default()
                    .to_owned(),
            }),
            session_id: self.session.session_id.to_string(),
            session_generation: self.session.session_generation,
            deadline_unix_ms: context.deadline_unix_ms,
            replay_identity: context.replay_identity.clone(),
            audit_correlation: context.audit_correlation.clone(),
        }
    }

    fn resource(reference: &o3k_kernel::ResourceReference) -> proto::ResourceRef {
        proto::ResourceRef {
            namespace: reference.resource_type.namespace().to_owned(),
            r#type: reference.resource_type.name().to_owned(),
            id: reference.resource_id.as_str().to_owned(),
            generation: reference.generation,
        }
    }
}

impl ExternalControllerClient {
    pub async fn connect(
        endpoint: &str,
        tls: tonic::transport::ClientTlsConfig,
    ) -> Result<Self, SdkError> {
        let channel = Endpoint::from_shared(endpoint.to_owned())
            .map_err(|error| SdkError::Invalid(error.to_string()))?
            .tls_config(tls)
            .map_err(|error| SdkError::Invalid(error.to_string()))?
            .connect()
            .await
            .map_err(|error| SdkError::Invalid(error.to_string()))?;
        Ok(Self {
            client: proto::controller_service_client::ControllerServiceClient::new(channel),
        })
    }
    pub async fn register(
        &mut self,
        request: proto::RegisterRequest,
    ) -> Result<proto::RegisterResponse, SdkError> {
        self.client
            .register(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn health(
        &mut self,
        request: proto::HealthRequest,
    ) -> Result<proto::HealthResponse, SdkError> {
        self.client
            .health(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn capabilities(
        &mut self,
        request: proto::CapabilitiesRequest,
    ) -> Result<proto::CapabilitiesResponse, SdkError> {
        self.client
            .capabilities(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn reconcile(
        &mut self,
        request: proto::ReconcileRequest,
    ) -> Result<proto::ReconcileResponse, SdkError> {
        self.client
            .reconcile(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn observe(
        &mut self,
        request: proto::ObserveRequest,
    ) -> Result<proto::ObserveResponse, SdkError> {
        self.client
            .observe(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub async fn delete(
        &mut self,
        request: proto::DeleteRequest,
    ) -> Result<proto::DeleteResponse, SdkError> {
        self.client
            .delete(request)
            .await
            .map(|response| response.into_inner())
            .map_err(|error| SdkError::Invalid(error.to_string()))
    }
}

fn delegation_wire(
    delegation: &o3k_kernel::DelegationContext,
) -> Result<proto::Delegation, SdkError> {
    let claims = DelegationClaims {
        version: 1,
        credential_id: delegation.credential_id,
        issuer: "o3k-control-plane".into(),
        key_id: delegation.key_id.clone(),
        original_actor: delegation.original_actor.clone(),
        owner_scope: delegation.original_scope.to_string(),
        calling_service: delegation.calling_service.name().to_owned(),
        recipient_service: delegation.recipient_service.name().to_owned(),
        action: delegation.delegated_action.to_string(),
        resource_type: delegation.resource.resource_type.to_string(),
        resource_id: delegation.resource.resource_id.as_str().to_owned(),
        request_id: delegation.request_id,
        operation_id: delegation.operation_id,
        session_id: delegation.session_id,
        session_generation: delegation.session_generation,
        issued_at_unix_ms: delegation.issued_at_unix_ms,
        expires_at_unix_ms: delegation.expires_at_unix_ms,
    };
    let signed = SignedDelegation {
        claims,
        signature: delegation.signature.clone(),
    };
    serde_json::to_vec(&signed)
        .map(|credential| proto::Delegation { credential })
        .map_err(|_| SdkError::Invalid("delegation serialization failed".into()))
}

fn optional_delegation(
    delegation: Option<&o3k_kernel::DelegationContext>,
) -> Result<Option<proto::Delegation>, SdkError> {
    delegation.map(delegation_wire).transpose()
}

fn failure_from_wire(failure: proto::Failure) -> o3k_kernel::ControllerFailure {
    let category = match proto::failure::Category::try_from(failure.category) {
        Ok(proto::failure::Category::InvalidRequest) => FailureCategory::InvalidRequest,
        Ok(proto::failure::Category::Unauthorized) => FailureCategory::Unauthorized,
        Ok(proto::failure::Category::Forbidden) => FailureCategory::Forbidden,
        Ok(proto::failure::Category::Conflict) => FailureCategory::Conflict,
        Ok(proto::failure::Category::StaleGeneration) => FailureCategory::StaleGeneration,
        Ok(proto::failure::Category::NotFound) => FailureCategory::NotFound,
        Ok(proto::failure::Category::NotReady) => FailureCategory::NotReady,
        Ok(proto::failure::Category::Retryable) => FailureCategory::Retryable,
        Ok(proto::failure::Category::NonRetryable) => FailureCategory::NonRetryable,
        Ok(proto::failure::Category::UnknownOutcome) => FailureCategory::UnknownOutcome,
        Ok(proto::failure::Category::Incompatible) => FailureCategory::Incompatible,
        Ok(proto::failure::Category::StaleSession) => FailureCategory::StaleSession,
        Ok(proto::failure::Category::ReplayConflict) => FailureCategory::ReplayConflict,
        Ok(proto::failure::Category::DelegationInvalid) => FailureCategory::DelegationInvalid,
        Ok(proto::failure::Category::ResourceExhausted) => FailureCategory::ResourceExhausted,
        Ok(proto::failure::Category::DeadlineExceeded) => FailureCategory::DeadlineExceeded,
        _ => FailureCategory::InvalidRequest,
    };
    o3k_kernel::ControllerFailure::new(category, failure.diagnostic)
}

fn observation_from_wire(
    observation: Option<proto::Observation>,
) -> Option<o3k_kernel::Observation> {
    observation.and_then(|value| {
        let resource = value.resource.as_ref().and_then(|resource| {
            Some(o3k_kernel::ResourceReference {
                resource_type: o3k_kernel::ResourceType::new(&resource.namespace, &resource.r#type)
                    .ok()?,
                resource_id: o3k_kernel::ResourceId::new(&resource.id).ok()?,
                generation: resource.generation,
            })
        })?;
        let status = if value.status.is_empty() {
            None
        } else {
            serde_json::from_slice(&value.status).ok()
        };
        Some(o3k_kernel::Observation {
            resource,
            exists: value.exists,
            observed_revision: (!value.observed_revision.is_empty())
                .then_some(value.observed_revision),
            status,
            diagnostics: (!value.diagnostics.is_empty()).then_some(value.diagnostics),
        })
    })
}

#[async_trait::async_trait]
impl o3k_kernel::Controller for GrpcControllerAdapter {
    async fn health(&self) -> o3k_kernel::ControllerHealth {
        let mut client = self.client.lock().await;
        let context = proto::HealthRequest {
            context: Some(proto::Context {
                service_id: self.service_id.clone(),
                session_id: self.session.session_id.to_string(),
                session_generation: self.session.session_generation,
                deadline_unix_ms: Self::health_deadline(),
                ..Default::default()
            }),
        };
        match client.health(context).await {
            Ok(response) => o3k_kernel::ControllerHealth {
                healthy: response.healthy,
                detail: (!response.detail.is_empty()).then_some(response.detail),
                protocol_version: o3k_kernel::ProtocolVersion::V1,
            },
            Err(error) => o3k_kernel::ControllerHealth {
                healthy: false,
                detail: Some(error.to_string()),
                protocol_version: o3k_kernel::ProtocolVersion::V1,
            },
        }
    }

    async fn capabilities(&self) -> o3k_kernel::ControllerCapabilities {
        let mut client = self.client.lock().await;
        match client
            .capabilities(proto::CapabilitiesRequest {
                context: Some(proto::Context {
                    service_id: self.service_id.clone(),
                    session_id: self.session.session_id.to_string(),
                    session_generation: self.session.session_generation,
                    deadline_unix_ms: Self::health_deadline(),
                    ..Default::default()
                }),
            })
            .await
        {
            Ok(response) => o3k_kernel::ControllerCapabilities {
                protocol_version: o3k_kernel::ProtocolVersion::V1,
                resource_types: response.resource_types,
                actions: response.actions,
            },
            Err(_) => o3k_kernel::ControllerCapabilities {
                protocol_version: o3k_kernel::ProtocolVersion::V1,
                resource_types: Vec::new(),
                actions: Vec::new(),
            },
        }
    }

    async fn reconcile(
        &self,
        request: o3k_kernel::ReconcileRequest,
    ) -> o3k_kernel::ReconcileOutcome {
        let delegation = match optional_delegation(request.delegation.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                return o3k_kernel::ReconcileOutcome::Failed {
                    failure: o3k_kernel::ControllerFailure::new(
                        FailureCategory::DelegationInvalid,
                        error.to_string(),
                    ),
                };
            }
        };
        let wire = proto::ReconcileRequest {
            context: Some(self.context(&request.context)),
            resource: Some(proto::Snapshot {
                resource: Some(Self::resource(&request.resource.reference)),
                desired_spec: serde_json::to_vec(&request.resource.desired_spec)
                    .unwrap_or_default(),
                known_status: request
                    .resource
                    .known_status
                    .as_ref()
                    .map(|value| serde_json::to_vec(value).unwrap_or_default())
                    .unwrap_or_default(),
                owner_scope: Some(
                    self.context(&request.context)
                        .owner_scope
                        .unwrap_or_default(),
                ),
            }),
            delegation,
        };
        let mut client = self.client.lock().await;
        match client.reconcile(wire).await {
            Ok(response) => match response.failure {
                Some(failure) => {
                    let failure = failure_from_wire(failure);
                    match failure.category {
                        FailureCategory::Retryable => {
                            o3k_kernel::ReconcileOutcome::Retryable { failure }
                        }
                        FailureCategory::UnknownOutcome => {
                            o3k_kernel::ReconcileOutcome::Unknown { failure }
                        }
                        _ => o3k_kernel::ReconcileOutcome::Failed { failure },
                    }
                }
                None => {
                    let observation = observation_from_wire(response.observation);
                    if response.accepted {
                        o3k_kernel::ReconcileOutcome::Accepted { observation }
                    } else {
                        o3k_kernel::ReconcileOutcome::Succeeded { observation }
                    }
                }
            },
            Err(error) => o3k_kernel::ReconcileOutcome::Unknown {
                failure: o3k_kernel::ControllerFailure::new(
                    FailureCategory::UnknownOutcome,
                    error.to_string(),
                ),
            },
        }
    }

    async fn observe(&self, request: o3k_kernel::ObserveRequest) -> o3k_kernel::ObserveOutcome {
        let delegation = match optional_delegation(request.delegation.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                return o3k_kernel::ObserveOutcome {
                    observation: None,
                    failure: Some(o3k_kernel::ControllerFailure::new(
                        FailureCategory::DelegationInvalid,
                        error.to_string(),
                    )),
                };
            }
        };
        let mut client = self.client.lock().await;
        match client
            .observe(proto::ObserveRequest {
                context: Some(self.context(&request.context)),
                resource: Some(Self::resource(&request.resource)),
                owner_scope: Some(
                    self.context(&request.context)
                        .owner_scope
                        .unwrap_or_default(),
                ),
                delegation,
            })
            .await
        {
            Ok(response) => o3k_kernel::ObserveOutcome {
                observation: observation_from_wire(response.observation),
                failure: response.failure.map(failure_from_wire),
            },
            Err(error) => o3k_kernel::ObserveOutcome {
                observation: None,
                failure: Some(o3k_kernel::ControllerFailure::new(
                    FailureCategory::Retryable,
                    error.to_string(),
                )),
            },
        }
    }

    async fn delete(&self, request: o3k_kernel::DeleteRequest) -> o3k_kernel::ReconcileOutcome {
        let delegation = match optional_delegation(request.delegation.as_ref()) {
            Ok(value) => value,
            Err(error) => {
                return o3k_kernel::ReconcileOutcome::Failed {
                    failure: o3k_kernel::ControllerFailure::new(
                        FailureCategory::DelegationInvalid,
                        error.to_string(),
                    ),
                };
            }
        };
        let mut client = self.client.lock().await;
        match client
            .delete(proto::DeleteRequest {
                context: Some(self.context(&request.context)),
                resource: Some(Self::resource(&request.resource)),
                owner_scope: Some(
                    self.context(&request.context)
                        .owner_scope
                        .unwrap_or_default(),
                ),
                delegation,
            })
            .await
        {
            Ok(response) => match response.failure {
                Some(failure) => o3k_kernel::ReconcileOutcome::Failed {
                    failure: failure_from_wire(failure),
                },
                None => {
                    let observation = observation_from_wire(response.observation);
                    if response.accepted {
                        o3k_kernel::ReconcileOutcome::Accepted { observation }
                    } else {
                        o3k_kernel::ReconcileOutcome::Succeeded { observation }
                    }
                }
            },
            Err(error) => o3k_kernel::ReconcileOutcome::Unknown {
                failure: o3k_kernel::ControllerFailure::new(
                    FailureCategory::UnknownOutcome,
                    error.to_string(),
                ),
            },
        }
    }
}

impl<H: ControllerHandler> ServiceControllerServer<H> {
    pub fn new(
        handler: H,
        service_id: impl Into<String>,
        namespace: impl Into<String>,
        manifest_digest: impl Into<String>,
        generation: u64,
    ) -> Self {
        let service_id = service_id.into();
        Self {
            handler: Arc::new(handler),
            service_id: service_id.clone(),
            namespace: namespace.into(),
            manifest_digest: manifest_digest.into(),
            generation,
            fence: SessionFence::default(),
            replay: ReplayLedger::default(),
            delegation_keys: Arc::new(HashMap::new()),
            expected_service_principal: None,
            expected_delegation_recipient: service_id,
        }
    }
    pub fn into_service(self) -> ControllerServiceServer<Self> {
        ControllerServiceServer::new(self)
    }
    pub fn with_delegation_keys(mut self, keys: HashMap<String, VerifyingKey>) -> Self {
        self.delegation_keys = Arc::new(keys);
        self
    }

    #[must_use]
    pub fn with_service_principal(mut self, principal: impl Into<String>) -> Self {
        self.expected_service_principal = Some(principal.into());
        self
    }

    #[must_use]
    pub fn with_delegation_recipient(mut self, recipient: impl Into<String>) -> Self {
        self.expected_delegation_recipient = recipient.into();
        self
    }
    #[allow(clippy::result_large_err)]
    fn validate_delegation(
        &self,
        delegation: Option<&proto::Delegation>,
        context: &proto::Context,
        resource: Option<&proto::ResourceRef>,
        now: u64,
    ) -> Result<(), tonic::Status> {
        let Some(delegation) = delegation else {
            return Ok(());
        };
        if self.delegation_keys.is_empty() {
            return Err(tonic::Status::permission_denied(
                "delegation verification keys are not configured",
            ));
        }
        let claims = verify_wire_delegation(&delegation.credential, &self.delegation_keys, now)
            .map_err(|_| tonic::Status::permission_denied("invalid delegation"))?;
        bind_delegation(
            &claims,
            context,
            resource,
            &self.expected_delegation_recipient,
        )
        .map(|_| ())
        .map_err(|_| tonic::Status::permission_denied("invalid delegation"))
    }
    async fn check_context(&self, context: Option<&proto::Context>) -> Result<u64, tonic::Status> {
        let context =
            context.ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        let session = Uuid::parse_str(&context.session_id)
            .map_err(|_| tonic::Status::unauthenticated("invalid session"))?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| tonic::Status::internal("clock error"))?
            .as_millis() as u64;
        if context.deadline_unix_ms <= now
            || context.deadline_unix_ms - now > o3k_kernel::controller::MAX_DEADLINE_MS
        {
            return Err(tonic::Status::deadline_exceeded(
                "invalid controller deadline",
            ));
        }
        self.fence
            .validate(&self.service_id, session, context.session_generation)
            .await
            .map_err(|_| tonic::Status::failed_precondition("stale session"))?;
        Ok(now)
    }
}

#[tonic::async_trait]
impl<H: ControllerHandler> ControllerService for ServiceControllerServer<H> {
    async fn register(
        &self,
        request: tonic::Request<proto::RegisterRequest>,
    ) -> Result<tonic::Response<proto::RegisterResponse>, tonic::Status> {
        let input = request.into_inner();
        if input.service_id != self.service_id
            || input.namespace != self.namespace
            || input.manifest_digest != self.manifest_digest
            || input.manifest_generation != self.generation
        {
            return Err(tonic::Status::permission_denied(
                "manifest identity mismatch",
            ));
        }
        if let Some(expected) = &self.expected_service_principal
            && input.service_principal_id != *expected
        {
            return Err(tonic::Status::permission_denied(
                "service principal identity mismatch",
            ));
        }
        let negotiated = input
            .supported_versions
            .into_iter()
            .find(|v| v.major == 1 && v.minor == 0)
            .ok_or_else(|| tonic::Status::failed_precondition("no compatible protocol version"))?;
        let session_id = Uuid::new_v4();
        self.fence
            .establish(SessionBinding {
                service_id: self.service_id.clone(),
                session_id,
                generation: 1,
            })
            .await;
        Ok(tonic::Response::new(proto::RegisterResponse {
            negotiated_version: Some(negotiated),
            session_id: session_id.to_string(),
            session_generation: 1,
        }))
    }
    async fn health(
        &self,
        request: tonic::Request<proto::HealthRequest>,
    ) -> Result<tonic::Response<proto::HealthResponse>, tonic::Status> {
        let input = request.into_inner();
        self.check_context(input.context.as_ref()).await?;
        Ok(tonic::Response::new(self.handler.health(input).await?))
    }
    async fn capabilities(
        &self,
        request: tonic::Request<proto::CapabilitiesRequest>,
    ) -> Result<tonic::Response<proto::CapabilitiesResponse>, tonic::Status> {
        let input = request.into_inner();
        self.check_context(input.context.as_ref()).await?;
        Ok(tonic::Response::new(
            self.handler.capabilities(input).await?,
        ))
    }
    async fn reconcile(
        &self,
        request: tonic::Request<proto::ReconcileRequest>,
    ) -> Result<tonic::Response<proto::ReconcileResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref().and_then(|s| s.resource.as_ref()),
            now,
        )?;
        self.replay
            .reserve(&context.replay_identity, &input.encode_to_vec())
            .await
            .map_err(|_| tonic::Status::aborted("conflicting replay identity"))?;
        Ok(tonic::Response::new(self.handler.reconcile(input).await?))
    }
    async fn observe(
        &self,
        request: tonic::Request<proto::ObserveRequest>,
    ) -> Result<tonic::Response<proto::ObserveResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref(),
            now,
        )?;
        Ok(tonic::Response::new(self.handler.observe(input).await?))
    }
    async fn delete(
        &self,
        request: tonic::Request<proto::DeleteRequest>,
    ) -> Result<tonic::Response<proto::DeleteResponse>, tonic::Status> {
        let input = request.into_inner();
        let now = self.check_context(input.context.as_ref()).await?;
        let context = input
            .context
            .as_ref()
            .ok_or_else(|| tonic::Status::invalid_argument("context is required"))?;
        self.validate_delegation(
            input.delegation.as_ref(),
            context,
            input.resource.as_ref(),
            now,
        )?;
        self.replay
            .reserve(&context.replay_identity, &input.encode_to_vec())
            .await
            .map_err(|_| tonic::Status::aborted("conflicting replay identity"))?;
        Ok(tonic::Response::new(self.handler.delete(input).await?))
    }
}

pub const MAX_REPLAY_ENTRIES: usize = 4096;

#[derive(Debug, Error)]
pub enum SdkError {
    #[error("controller is not registered")]
    NotRegistered,
    #[error("stale controller session")]
    StaleSession,
    #[error("conflicting replay identity")]
    ReplayConflict,
    #[error("invalid request: {0}")]
    Invalid(String),
    #[error("delegation signature is invalid")]
    InvalidDelegation,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DelegationClaims {
    pub version: u8,
    pub credential_id: Uuid,
    pub issuer: String,
    pub key_id: String,
    pub original_actor: String,
    pub owner_scope: String,
    pub calling_service: String,
    pub recipient_service: String,
    pub action: String,
    pub resource_type: String,
    pub resource_id: String,
    pub request_id: Uuid,
    pub operation_id: Uuid,
    pub session_id: Uuid,
    pub session_generation: u64,
    pub issued_at_unix_ms: u64,
    pub expires_at_unix_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SignedDelegation {
    pub claims: DelegationClaims,
    pub signature: Vec<u8>,
}

pub fn verify_wire_delegation(
    bytes: &[u8],
    keys: &HashMap<String, VerifyingKey>,
    now_unix_ms: u64,
) -> Result<DelegationClaims, SdkError> {
    if bytes.is_empty() || bytes.len() > o3k_controller_protocol::MAX_DELEGATION_BYTES {
        return Err(SdkError::InvalidDelegation);
    }
    let credential: SignedDelegation =
        serde_json::from_slice(bytes).map_err(|_| SdkError::InvalidDelegation)?;
    Ok(credential.verify(keys, now_unix_ms)?.clone())
}

/// Bind an already cryptographically verified credential to the exact wire
/// request. Signature validity alone never grants authority for a different
/// action, scope, operation, resource, or controller session.
pub fn bind_delegation(
    claims: &DelegationClaims,
    context: &proto::Context,
    resource: Option<&proto::ResourceRef>,
    recipient_service: &str,
) -> Result<(), SdkError> {
    let request_id =
        Uuid::parse_str(&context.request_id).map_err(|_| SdkError::InvalidDelegation)?;
    let operation_id =
        Uuid::parse_str(&context.operation_id).map_err(|_| SdkError::InvalidDelegation)?;
    let session_id =
        Uuid::parse_str(&context.session_id).map_err(|_| SdkError::InvalidDelegation)?;
    if claims.request_id != request_id
        || claims.operation_id != operation_id
        || claims.session_id != session_id
        || claims.session_generation != context.session_generation
        || claims.calling_service != context.service_id
        || claims.recipient_service != recipient_service
        || claims.action != context.action
    {
        return Err(SdkError::InvalidDelegation);
    }
    let scope = context
        .owner_scope
        .as_ref()
        .ok_or(SdkError::InvalidDelegation)?;
    let kind = proto::scope::Kind::try_from(scope.kind).map_err(|_| SdkError::InvalidDelegation)?;
    let kind = match kind {
        proto::scope::Kind::Project => "project",
        proto::scope::Kind::Domain => "domain",
        proto::scope::Kind::System => "system",
        proto::scope::Kind::Unspecified => return Err(SdkError::InvalidDelegation),
    };
    if claims.owner_scope != format!("{kind}:{}", scope.id) {
        return Err(SdkError::InvalidDelegation);
    }
    if let Some(resource) = resource {
        let resource_name = format!("{}:{}", resource.namespace, resource.r#type);
        if claims.resource_type != resource_name
            || claims.resource_id != resource.id
            || claims.resource_id.is_empty()
            || resource.generation < 0
        {
            return Err(SdkError::InvalidDelegation);
        }
    }
    Ok(())
}

impl SignedDelegation {
    fn signing_bytes(claims: &DelegationClaims) -> Result<Vec<u8>, SdkError> {
        serde_json::to_vec(claims).map_err(|error| SdkError::Invalid(error.to_string()))
    }
    pub fn sign(claims: DelegationClaims, key: &SigningKey) -> Result<Self, SdkError> {
        let signature = key.sign(&Self::signing_bytes(&claims)?).to_bytes().to_vec();
        Ok(Self { claims, signature })
    }
    pub fn verify(
        &self,
        keys: &std::collections::HashMap<String, VerifyingKey>,
        now_unix_ms: u64,
    ) -> Result<&DelegationClaims, SdkError> {
        if self.claims.version != 1
            || self.signature.len() != 64
            || now_unix_ms < self.claims.issued_at_unix_ms
            || now_unix_ms >= self.claims.expires_at_unix_ms
        {
            return Err(SdkError::InvalidDelegation);
        }
        let key = keys
            .get(&self.claims.key_id)
            .ok_or(SdkError::InvalidDelegation)?;
        let signature =
            Signature::from_slice(&self.signature).map_err(|_| SdkError::InvalidDelegation)?;
        key.verify(&Self::signing_bytes(&self.claims)?, &signature)
            .map_err(|_| SdkError::InvalidDelegation)?;
        Ok(&self.claims)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionBinding {
    pub service_id: String,
    pub session_id: Uuid,
    pub generation: u64,
}

#[derive(Clone, Default)]
pub struct SessionFence {
    current: Arc<Mutex<Option<SessionBinding>>>,
}

impl SessionFence {
    pub async fn establish(&self, binding: SessionBinding) {
        *self.current.lock().await = Some(binding);
    }
    pub async fn validate(
        &self,
        service_id: &str,
        session_id: Uuid,
        generation: u64,
    ) -> Result<(), SdkError> {
        match self.current.lock().await.as_ref() {
            Some(current)
                if current.service_id == service_id
                    && current.session_id == session_id
                    && current.generation == generation =>
            {
                Ok(())
            }
            _ => Err(SdkError::StaleSession),
        }
    }
}

#[derive(Clone, Default)]
pub struct ReplayLedger {
    entries: Arc<Mutex<HashMap<String, [u8; 32]>>>,
}

impl ReplayLedger {
    pub async fn reserve(&self, key: &str, canonical_request: &[u8]) -> Result<(), SdkError> {
        let digest: [u8; 32] = Sha256::digest(canonical_request).into();
        let mut entries = self.entries.lock().await;
        if let Some(previous) = entries.get(key) {
            return if previous == &digest {
                Ok(())
            } else {
                Err(SdkError::ReplayConflict)
            };
        }
        if entries.len() >= MAX_REPLAY_ENTRIES {
            return Err(SdkError::Invalid("replay ledger capacity exhausted".into()));
        }
        entries.insert(key.to_owned(), digest);
        Ok(())
    }
}

pub fn validate_context(
    context: &OperationContext,
    now_unix_ms: u64,
) -> Result<(), ControllerFailure> {
    if context.service_id.trim().is_empty() {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "service identity is required",
        ));
    }
    if context.replay_identity.trim().is_empty() {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "replay identity is required",
        ));
    }
    if context.deadline_unix_ms <= now_unix_ms {
        return Err(ControllerFailure::new(
            FailureCategory::DeadlineExceeded,
            "deadline has expired",
        ));
    }
    if context.deadline_unix_ms - now_unix_ms > o3k_kernel::controller::MAX_DEADLINE_MS {
        return Err(ControllerFailure::new(
            FailureCategory::InvalidRequest,
            "deadline exceeds maximum",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;
    #[tokio::test]
    async fn equivalent_replay_is_allowed_and_conflict_is_rejected() {
        let ledger = ReplayLedger::default();
        assert!(ledger.reserve("op-1", b"same").await.is_ok());
        assert!(ledger.reserve("op-1", b"same").await.is_ok());
        assert!(matches!(
            ledger.reserve("op-1", b"changed").await,
            Err(SdkError::ReplayConflict)
        ));
    }

    #[test]
    fn composition_transport_preserves_mutation_uncertainty() {
        let unknown = tonic::Status::unknown("transport lost after dispatch");
        assert!(matches!(
            composition::composition_transport_error(unknown, true),
            composition::CompositionError::UnknownOutcome
        ));

        let unavailable = tonic::Status::unavailable("controller unavailable");
        assert!(matches!(
            composition::composition_transport_error(unavailable, true),
            composition::CompositionError::UnknownOutcome
        ));

        let denied = tonic::Status::permission_denied("denied");
        assert!(matches!(
            composition::composition_transport_error(denied, true),
            composition::CompositionError::Unauthorized
        ));
    }
    #[tokio::test]
    async fn replacement_fences_old_session() {
        let fence = SessionFence::default();
        let old = SessionBinding {
            service_id: "svc".into(),
            session_id: Uuid::new_v4(),
            generation: 1,
        };
        let new = SessionBinding {
            service_id: "svc".into(),
            session_id: Uuid::new_v4(),
            generation: 2,
        };
        fence.establish(old.clone()).await;
        assert!(fence.validate("svc", old.session_id, 1).await.is_ok());
        fence.establish(new.clone()).await;
        assert!(matches!(
            fence.validate("svc", old.session_id, 1).await,
            Err(SdkError::StaleSession)
        ));
        assert!(fence.validate("svc", new.session_id, 2).await.is_ok());
    }

    #[test]
    fn delegation_tampering_and_expiry_fail_closed() -> Result<(), SdkError> {
        let signing = SigningKey::from_bytes(&[7; 32]);
        let claims = DelegationClaims {
            version: 1,
            credential_id: Uuid::new_v4(),
            issuer: "o3k".into(),
            key_id: "k1".into(),
            original_actor: "user".into(),
            owner_scope: "project:p1".into(),
            calling_service: "svc-a".into(),
            recipient_service: "svc-b".into(),
            action: "compute:ReadServer".into(),
            resource_type: "compute:server".into(),
            resource_id: "s1".into(),
            request_id: Uuid::new_v4(),
            operation_id: Uuid::new_v4(),
            session_id: Uuid::new_v4(),
            session_generation: 1,
            issued_at_unix_ms: 10,
            expires_at_unix_ms: 20,
        };
        let mut signed = SignedDelegation::sign(claims, &signing)?;
        let mut keys = HashMap::new();
        keys.insert("k1".into(), signing.verifying_key());
        assert!(signed.verify(&keys, 15).is_ok());
        signed.claims.action = "compute:DeleteServer".into();
        assert!(matches!(
            signed.verify(&keys, 15),
            Err(SdkError::InvalidDelegation)
        ));
        let signed = SignedDelegation::sign(signed.claims, &signing)?;
        assert!(matches!(
            signed.verify(&keys, 20),
            Err(SdkError::InvalidDelegation)
        ));
        Ok(())
    }
}
