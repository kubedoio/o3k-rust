//! Generic native resource application boundary.
//!
//! This module deliberately contains no provider or controller types.  Native
//! adapters resolve a validated descriptor and hand the request to this port;
//! the implementation below the port owns canonical resources, operations and
//! idempotency.
#![allow(clippy::items_after_test_module)]

use base64::Engine as _;
use std::{collections::HashMap, sync::Arc};

use crate::pagination::{CursorPayload, parse_page_size};
use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};
use async_trait::async_trait;
use axum::{
    Json,
    body::Bytes,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, ResourceTarget,
    ResourceType,
};
use serde::{
    Deserialize, Serialize,
    de::{self, SeqAccess, Visitor},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleOperation {
    Create,
    Delete,
    List,
    Show,
    Update,
}

#[derive(Debug, Clone)]
pub struct ResourceDescriptor {
    pub resource_type: ResourceType,
    pub collection: String,
    pub schema_version: String,
    pub scope: o3k_kernel::ResourceScope,
    pub lifecycle_actions: HashMap<LifecycleOperation, ActionId>,
    pub owning_service: String,
    pub ownership: o3k_kernel::ServiceOwnership,
    pub ready: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DescriptorError {
    EmptyCollection,
    DuplicateCollection,
    ReservedCollection,
    MissingAction(LifecycleOperation),
    InvalidAction,
    InvalidOperation,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceDispatcher {
    descriptors: HashMap<(String, String), ResourceDescriptor>,
    lifecycle_registry: Option<Arc<std::sync::RwLock<o3k_kernel::ManifestRegistry>>>,
}

impl ResourceDispatcher {
    /// Builds the dispatch index from the canonical manifest registry. This
    /// index is derived state and is never independently registered by API
    /// callers.
    pub fn from_manifest_registry(
        manifests: &o3k_kernel::ManifestRegistry,
    ) -> Result<Self, DescriptorError> {
        Self::from_shared_manifest_registry(Arc::new(std::sync::RwLock::new(manifests.clone())))
    }

    /// Builds the dispatch index while retaining the shared lifecycle state.
    /// Descriptor metadata is static, but readiness is resolved from the
    /// registry for every discovery and mutation request.
    pub fn from_shared_manifest_registry(
        lifecycle_registry: Arc<std::sync::RwLock<o3k_kernel::ManifestRegistry>>,
    ) -> Result<Self, DescriptorError> {
        let mut index = Self::default();
        let manifests = lifecycle_registry
            .read()
            .map_err(|_| DescriptorError::InvalidOperation)?;
        for manifest in manifests.all() {
            let ready = manifests
                .controller(&manifest.service_id)
                .is_some_and(|c| c.state == o3k_kernel::controller::ControllerState::Ready);
            for resource in &manifest.resource_types {
                index.register(ResourceDescriptor {
                    resource_type: resource.resource_type.clone(),
                    collection: resource
                        .collection
                        .clone()
                        .unwrap_or_else(|| resource.resource_type.name().to_owned()),
                    schema_version: resource.schema_version.clone(),
                    scope: resource.scope,
                    lifecycle_actions: resource
                        .operations
                        .iter()
                        .map(|(operation, action)| {
                            let operation = match operation.as_str() {
                                "list" => LifecycleOperation::List,
                                "show" => LifecycleOperation::Show,
                                "create" => LifecycleOperation::Create,
                                "delete" => LifecycleOperation::Delete,
                                "update" => LifecycleOperation::Update,
                                _ => return Err(DescriptorError::InvalidOperation),
                            };
                            Ok((operation, action.clone()))
                        })
                        .collect::<Result<_, _>>()?,
                    owning_service: manifest.service_id.clone(),
                    ownership: manifest.ownership,
                    ready,
                })?;
            }
        }
        drop(manifests);
        index.lifecycle_registry = Some(lifecycle_registry);
        Ok(index)
    }

    fn register(&mut self, descriptor: ResourceDescriptor) -> Result<(), DescriptorError> {
        if descriptor.collection.trim().is_empty() {
            return Err(DescriptorError::EmptyCollection);
        }
        if ["services", "resource-types", "identity", "operations"]
            .contains(&descriptor.collection.as_str())
        {
            return Err(DescriptorError::ReservedCollection);
        }
        for action in descriptor.lifecycle_actions.values() {
            if action.namespace() != descriptor.resource_type.namespace() {
                return Err(DescriptorError::InvalidAction);
            }
        }
        let key = (
            descriptor.resource_type.namespace().to_owned(),
            descriptor.collection.clone(),
        );
        if self.descriptors.insert(key, descriptor).is_some() {
            return Err(DescriptorError::DuplicateCollection);
        }
        Ok(())
    }

    #[must_use]
    pub fn resolve(&self, namespace: &str, collection: &str) -> Option<&ResourceDescriptor> {
        self.descriptors
            .get(&(namespace.to_owned(), collection.to_owned()))
    }

    #[must_use]
    pub fn resolve_resource_type(
        &self,
        resource_type: &ResourceType,
    ) -> Option<&ResourceDescriptor> {
        self.descriptors
            .values()
            .find(|descriptor| descriptor.resource_type == *resource_type)
    }
    pub fn all(&self) -> impl Iterator<Item = &ResourceDescriptor> {
        self.descriptors.values()
    }

    /// Return at most `limit` descriptors without materializing the complete
    /// dispatch index.  Public discovery callers request one extra entry to
    /// detect overflow and fail closed before constructing a response.
    #[must_use]
    pub fn all_bounded(&self, limit: usize) -> Vec<&ResourceDescriptor> {
        self.descriptors.values().take(limit).collect()
    }

    pub(crate) fn is_ready(&self, descriptor: &ResourceDescriptor) -> bool {
        let Some(registry) = &self.lifecycle_registry else {
            return descriptor.ready;
        };
        registry
            .read()
            .ok()
            .and_then(|registry| {
                registry
                    .controller(&descriptor.owning_service)
                    .map(|c| c.state)
            })
            .is_some_and(|state| state == o3k_kernel::controller::ControllerState::Ready)
    }

    pub(crate) fn declared_action(
        &self,
        descriptor: &ResourceDescriptor,
        name: &str,
    ) -> Option<ActionId> {
        let registry = self.lifecycle_registry.as_ref()?.read().ok()?;
        let manifest = registry.get(&descriptor.owning_service)?;
        let wire = format!("{}:{name}", descriptor.resource_type.namespace());
        manifest
            .actions
            .iter()
            .find(|declared| *declared == &wire)?;
        ActionId::new(descriptor.resource_type.namespace(), name).ok()
    }

    /// Resolve an authoritative lifecycle action from the manifest-derived
    /// descriptor. Callers must not construct action names from resource
    /// names; an undeclared lifecycle operation is unsupported.
    #[must_use]
    pub fn lifecycle_action(
        &self,
        namespace: &str,
        collection: &str,
        operation: LifecycleOperation,
    ) -> Option<&ActionId> {
        self.resolve(namespace, collection)
            .and_then(|descriptor| descriptor.lifecycle_actions.get(&operation))
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateRequest {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    #[serde(default)]
    pub spec: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpdateRequest {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    #[serde(default)]
    pub spec: serde_json::Value,
}

/// A create request after the resource-specific public contract has been
/// checked.  Applications never receive an unvalidated wire `Value`.
#[derive(Debug, Clone)]
pub struct ValidatedCreateRequest {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    pub spec: crate::resource_contract::ValidatedSpec,
}

#[derive(Debug, Clone)]
pub struct ValidatedUpdateRequest {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    pub spec: crate::resource_contract::ValidatedSpec,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResourceApplicationError {
    Unauthorized,
    Forbidden,
    NotFound,
    UnsupportedOperation,
    Conflict,
    PreconditionConflict,
    IdempotencyConflict,
    Validation,
    NotReady,
    Retryable,
    Internal,
}

#[derive(Debug, Clone, Serialize)]
pub struct MutationResult {
    pub operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    pub complete: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource: Option<serde_json::Value>,
}

/// Bounded result at the application boundary.  Implementations must fetch
/// at most `limit + 1` records and use the extra record only to determine
/// whether a continuation exists; callers never receive an unbounded
/// collection from the HTTP application.
#[derive(Debug, Clone)]
pub struct ResourcePage {
    pub items: Vec<serde_json::Value>,
    pub has_more: bool,
}

impl ResourcePage {
    /// Enforce the application boundary before any HTTP projection work.
    /// Implementations are expected to query `page_size + 1`; this defensive
    /// normalization keeps an accidentally over-sized adapter result bounded
    /// and preserves the continuation signal.
    pub fn bounded(mut self, page_size: usize) -> Self {
        let has_more = self.has_more || self.items.len() > page_size;
        self.items.truncate(page_size.saturating_add(1));
        self.has_more = has_more;
        self
    }
}

/// Remove fields which are never part of a tenant-facing resource contract.
/// Resource applications may be backed by an external controller, so the
/// HTTP boundary must remain safe even when that controller returns an
/// over-inclusive document.  This is deliberately deny-by-name and recursive;
/// public resource schemas remain the authoritative allow-list for normal
/// native resources.
pub(crate) fn public_value(value: serde_json::Value) -> serde_json::Value {
    fn forbidden(key: &str) -> bool {
        // Treat separator variants uniformly.  Provider/controller payloads
        // are not trusted to use one naming convention; in particular a
        // spaced or punctuated spelling must not bypass the deny list.
        let key = key
            .to_ascii_lowercase()
            .chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .collect::<String>();
        [
            "password",
            "token",
            "secret",
            "privatekey",
            "credential",
            "chap",
            "userdata",
            "userdatapayload",
            "providerid",
            "providerreference",
            "providerresourceid",
            "provideroperationid",
            "providerpath",
            "providerhost",
            "backend",
            "backendid",
            "nodeid",
            "hostpath",
            "connection",
            "connectionstring",
            "devicepath",
            "rawenvironment",
            "environment",
        ]
        .iter()
        .any(|needle| key.contains(needle))
    }
    match value {
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                .filter(|(key, _)| !forbidden(key))
                .map(|(key, value)| (key, public_value(value)))
                .collect(),
        ),
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.into_iter().map(public_value).collect())
        }
        other => other,
    }
}

fn public_mutation(mut result: MutationResult) -> MutationResult {
    result.resource = result.resource.map(public_value);
    result
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActionRequest {
    #[serde(default = "empty_action_input")]
    pub input: serde_json::Value,
    /// Optional bounded raw action payload.  This is intentionally not part
    /// of the JSON representation: binary actions negotiate their media type
    /// at the HTTP boundary and domain handlers validate the bytes.
    #[serde(skip)]
    pub payload: Option<Bytes>,
}

fn empty_action_input() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

#[derive(Debug, Clone, Serialize)]
pub struct RelationshipView {
    pub slot: String,
    pub resource_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_id: Option<String>,
    pub ownership: String,
    pub state: String,
    pub parent_operation_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub child_operation_id: Option<String>,
}

#[async_trait]
pub trait ResourceApplication: Send + Sync {
    async fn create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        request: ValidatedCreateRequest,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError>;
    async fn delete(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        idempotency_key: Option<&str>,
        expected_generation: Option<i64>,
    ) -> Result<MutationResult, ResourceApplicationError>;
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        query: &ListQuery,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError>;
    /// Bounded list contract. Every implementation must push scope, filtering,
    /// ordering, and the page bound through its repository boundary. There is
    /// deliberately no fallback to `list`: such a fallback would make an
    /// otherwise valid application capable of loading an unbounded collection.
    async fn list_page(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        query: &ListQuery,
    ) -> Result<ResourcePage, ResourceApplicationError>;
    async fn validate_list_cursor(
        &self,
        _descriptor: &ResourceDescriptor,
        _auth: &AuthContext,
        _cursor_id: &str,
    ) -> Result<bool, ResourceApplicationError> {
        Ok(true)
    }
    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError>;
    async fn relationships(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        limit: usize,
    ) -> Result<Vec<RelationshipView>, ResourceApplicationError> {
        let _ = (descriptor, auth, id, limit);
        Err(ResourceApplicationError::UnsupportedOperation)
    }
    async fn relationships_page(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        after_slot: Option<&str>,
        limit: usize,
    ) -> Result<Vec<RelationshipView>, ResourceApplicationError> {
        // Never fall back to the legacy whole-collection method here.  This
        // is a public collection boundary and a default fallback would allow
        // an otherwise valid adapter to load an unbounded relationship set
        // before the HTTP layer can enforce its page size.  Adapters that
        // expose relationships must implement the repository-bounded page
        // contract explicitly (including the continuation anchor).
        let _ = (descriptor, auth, id, after_slot, limit);
        Err(ResourceApplicationError::UnsupportedOperation)
    }
    async fn update(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        request: ValidatedUpdateRequest,
        idempotency_key: Option<&str>,
        expected_generation: i64,
    ) -> Result<MutationResult, ResourceApplicationError> {
        let _ = (
            descriptor,
            auth,
            id,
            request,
            idempotency_key,
            expected_generation,
        );
        Err(ResourceApplicationError::UnsupportedOperation)
    }

    async fn action(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        action: ActionId,
        request: ActionRequest,
        idempotency_key: &str,
    ) -> Result<MutationResult, ResourceApplicationError> {
        let _ = (descriptor, auth, id, action, request, idempotency_key);
        Err(ResourceApplicationError::UnsupportedOperation)
    }
}

/// Canonical native attachment orchestration supplied by the composition
/// root. The resource application persists the intent and delegates the
/// provider crossing to this restartable workflow.
#[async_trait]
pub trait VolumeAttachmentWorkflow: Send + Sync {
    async fn attach(&self, attachment_id: uuid::Uuid) -> Result<(), String>;
    async fn detach(&self, attachment_id: uuid::Uuid) -> Result<(), String>;
}

pub type SharedResourceApplication = Arc<dyn ResourceApplication>;

fn application_problem(error: ResourceApplicationError) -> Response {
    let code = match error {
        ResourceApplicationError::Unauthorized => ErrorCode::Unauthorized,
        ResourceApplicationError::Forbidden => ErrorCode::Forbidden,
        ResourceApplicationError::NotFound => ErrorCode::ResourceNotFound,
        ResourceApplicationError::UnsupportedOperation => ErrorCode::UnsupportedOperation,
        ResourceApplicationError::Conflict
        | ResourceApplicationError::PreconditionConflict
        | ResourceApplicationError::IdempotencyConflict => ErrorCode::Conflict,
        ResourceApplicationError::Validation => ErrorCode::BadRequest,
        ResourceApplicationError::NotReady => ErrorCode::NotAvailable,
        ResourceApplicationError::Retryable | ResourceApplicationError::Internal => {
            ErrorCode::InternalError
        }
    };
    ProblemDetails::new(code).into_response()
}

fn declared_action(
    descriptor: &ResourceDescriptor,
    operation: LifecycleOperation,
) -> Result<&ActionId, ErrorCode> {
    descriptor
        .lifecycle_actions
        .get(&operation)
        .ok_or(ErrorCode::UnsupportedOperation)
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<&str>, ErrorCode> {
    let Some(value) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| ErrorCode::BadRequest)?;
    if value.is_empty() || value.len() > 128 {
        return Err(ErrorCode::BadRequest);
    }
    Ok(Some(value))
}

fn ready_for_mutation(
    dispatcher: &ResourceDispatcher,
    descriptor: &ResourceDescriptor,
) -> Result<(), ErrorCode> {
    if dispatcher.is_ready(descriptor) {
        Ok(())
    } else {
        Err(ErrorCode::NotAvailable)
    }
}

fn authorize(
    state: &NativeApiState,
    descriptor: &ResourceDescriptor,
    action: &ActionId,
    auth: &AuthContext,
    id: Option<&str>,
) -> Result<(), ErrorCode> {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return Err(ErrorCode::Forbidden);
    };
    let target = match id {
        Some(id) => ResourceTarget::instance(
            descriptor.resource_type.clone(),
            o3k_kernel::ResourceId::new_unchecked(id),
            Some(auth.effective_scope().id().clone()),
        ),
        None => ResourceTarget::collection(
            descriptor.resource_type.clone(),
            Some(auth.effective_scope().id().clone()),
        ),
    };
    match authorizer.authorize(&AuthorizationRequest {
        auth_context: auth,
        action: action.clone(),
        resource_target: target,
    }) {
        AuthorizationDecision::Allow => Ok(()),
        AuthorizationDecision::Deny { .. } => Err(ErrorCode::Forbidden),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::items_after_test_module)]
mod tests {
    use super::*;

    #[test]
    fn action_request_rejects_undeclared_wire_fields() {
        let result = serde_json::from_str::<ActionRequest>(
            r#"{"input":{},"provider_credentials":"secret"}"#,
        );
        assert!(result.is_err());
    }

    fn descriptor(namespace: &str, name: &str, collection: &str) -> ResourceDescriptor {
        let mut lifecycle_actions = HashMap::new();
        for (operation, action) in [
            (LifecycleOperation::Create, "Create"),
            (LifecycleOperation::Delete, "Delete"),
            (LifecycleOperation::List, "List"),
            (LifecycleOperation::Show, "Show"),
        ] {
            lifecycle_actions.insert(operation, ActionId::new_unchecked(namespace, action));
        }
        ResourceDescriptor {
            resource_type: ResourceType::new_unchecked(namespace, name),
            collection: collection.into(),
            schema_version: "v1".into(),
            scope: o3k_kernel::ResourceScope::Tenant,
            lifecycle_actions,
            owning_service: namespace.into(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            ready: true,
        }
    }

    #[test]
    fn different_resource_types_share_one_registry_resolution_path() {
        let mut registry = ResourceDispatcher::default();
        registry
            .register(descriptor("compute", "server", "servers"))
            .unwrap();
        registry
            .register(descriptor("network", "endpoint", "endpoints"))
            .unwrap();
        assert_eq!(
            registry
                .resolve("compute", "servers")
                .unwrap()
                .resource_type
                .name(),
            "server"
        );
        assert_eq!(
            registry
                .resolve("network", "endpoints")
                .unwrap()
                .resource_type
                .name(),
            "endpoint"
        );
        assert!(registry.resolve("unknown", "servers").is_none());
    }

    #[test]
    fn relationship_list_projection_matches_versioned_schema() {
        let relationship = RelationshipView {
            slot: "root".into(),
            resource_type: "network:network".into(),
            resource_id: Some("network-1".into()),
            ownership: "exclusive".into(),
            state: "bound".into(),
            parent_operation_id: "operation-1".into(),
            child_operation_id: None,
        };
        let payload = serde_json::json!({
            "items": [relationship],
            "next_cursor": null
        });
        let schema: serde_json::Value = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../contracts/native-relationship-list-response-v1.schema.json"
        )))
        .unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        assert!(validator.validate(&payload).is_ok());
    }

    #[test]
    fn lifecycle_actions_are_descriptor_authoritative() {
        let mut registry = ResourceDispatcher::default();
        let mut value = descriptor("network", "network_intent", "network-intents");
        value.lifecycle_actions.remove(&LifecycleOperation::Delete);
        registry.register(value).unwrap();
        assert_eq!(
            registry
                .lifecycle_action("network", "network-intents", LifecycleOperation::Create)
                .map(ToString::to_string),
            Some("network:Create".to_owned())
        );
        assert!(
            registry
                .lifecycle_action("network", "network-intents", LifecycleOperation::Delete)
                .is_none()
        );
    }

    #[test]
    fn registry_rejects_reserved_and_ambiguous_collections() {
        let mut registry = ResourceDispatcher::default();
        assert_eq!(
            registry.register(descriptor("compute", "server", "operations")),
            Err(DescriptorError::ReservedCollection)
        );
        registry
            .register(descriptor("compute", "server", "servers"))
            .unwrap();
        assert_eq!(
            registry.register(descriptor("compute", "flavor", "servers")),
            Err(DescriptorError::DuplicateCollection)
        );
    }

    #[test]
    fn registry_allows_partial_lifecycle_actions() {
        let mut registry = ResourceDispatcher::default();
        let mut d = descriptor("compute", "server", "servers");
        d.lifecycle_actions.remove(&LifecycleOperation::Delete);
        assert!(registry.register(d).is_ok());
    }

    #[test]
    fn list_filter_shape_is_bounded_before_repository_dispatch() {
        assert_eq!(
            validate_filter_shape(&vec![
                "observed_state=ready".to_owned();
                MAX_LIST_FILTERS + 1
            ]),
            Err(ErrorCode::BadRequest)
        );
        assert_eq!(
            validate_filter_shape(&["x".repeat(MAX_LIST_FILTER_LENGTH + 1)]),
            Err(ErrorCode::BadRequest)
        );
        assert!(validate_filter_shape(&["observed_state=ready".to_owned()]).is_ok());
    }

    #[test]
    fn resource_page_contract_caps_adapter_results_to_page_size_plus_one() {
        let page = ResourcePage {
            items: (0..128)
                .map(|id| serde_json::json!({"metadata": {"id": id}}))
                .collect(),
            has_more: false,
        }
        .bounded(7);
        assert_eq!(page.items.len(), 8);
        assert!(page.has_more);
    }

    #[test]
    fn public_projection_rejects_spaced_secret_and_provider_keys() {
        let value = public_value(serde_json::json!({
            "private key": "must not escape",
            "provider id": "backend-id",
            "safe label": "ok",
        }));
        assert!(value.get("private key").is_none());
        assert!(value.get("provider id").is_none());
        assert_eq!(value["safe label"], "ok");
    }

    #[test]
    fn mutation_readiness_tracks_shared_lifecycle_transition() {
        let mut manifests = o3k_kernel::ManifestRegistry::new();
        manifests.seed_core().unwrap();
        manifests
            .register_in_process_controller("compute", true, None)
            .unwrap();
        let shared = Arc::new(std::sync::RwLock::new(manifests));
        let dispatcher = ResourceDispatcher::from_shared_manifest_registry(shared.clone()).unwrap();
        let descriptor = dispatcher.resolve("compute", "servers").unwrap();

        assert_eq!(ready_for_mutation(&dispatcher, descriptor), Ok(()));

        shared
            .write()
            .unwrap()
            .update_controller_health(
                "compute",
                o3k_kernel::controller::ControllerHealth {
                    healthy: false,
                    detail: Some("provider unavailable".to_owned()),
                    protocol_version: o3k_kernel::controller::ProtocolVersion::V1,
                },
            )
            .unwrap();

        assert_eq!(
            ready_for_mutation(&dispatcher, descriptor),
            Err(ErrorCode::NotAvailable)
        );
    }
}

pub async fn create(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection)): Path<(String, String)>,
    State(state): State<NativeApiState>,
    Json(request): Json<CreateRequest>,
) -> Response {
    create_for(
        auth,
        headers,
        namespace,
        collection,
        State(state),
        Json(request),
    )
    .await
}

pub async fn update(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection, id)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
    Json(request): Json<UpdateRequest>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::Update) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, Some(&id)) {
        return ProblemDetails::new(response).into_response();
    }
    if let Err(response) = ready_for_mutation(&state.resource_index, descriptor) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let key = match idempotency_key(&headers) {
        Ok(Some(key)) => Some(key),
        Ok(None) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    let expected_generation = match headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
    {
        Some(value) => match value
            .strip_prefix("generation-")
            .and_then(|v| v.parse::<i64>().ok())
        {
            Some(generation) if generation > 0 => generation,
            _ => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
        },
        None => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let spec = match crate::resource_contract::ContractKind::for_resource(
        &descriptor.resource_type.to_string(),
        &descriptor.schema_version,
    ) {
        Some(kind) => match kind.validate_update(request.spec) {
            Ok(spec) => spec,
            Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
        },
        None => return ProblemDetails::new(ErrorCode::UnsupportedOperation).into_response(),
    };
    match application
        .update(
            descriptor,
            &auth.0,
            &id,
            ValidatedUpdateRequest {
                api_version: request.api_version,
                kind: request.kind,
                spec,
            },
            key,
            expected_generation,
        )
        .await
    {
        Ok(result) if result.complete => {
            (StatusCode::OK, Json(public_mutation(result))).into_response()
        }
        Ok(result) => (StatusCode::ACCEPTED, Json(public_mutation(result))).into_response(),
        Err(error) => application_problem(error),
    }
}

pub async fn action(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection, id, action_name)): Path<(String, String, String, String)>,
    State(state): State<NativeApiState>,
    body: Bytes,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response();
    };
    let Some(action) = state
        .resource_index
        .declared_action(descriptor, &action_name)
    else {
        return ProblemDetails::new(ErrorCode::UnsupportedOperation).into_response();
    };
    if let Err(response) = authorize(&state, descriptor, &action, &auth.0, Some(&id)) {
        return ProblemDetails::new(response).into_response();
    }
    if let Err(response) = ready_for_mutation(&state.resource_index, descriptor) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    let content_type = headers
        .get(axum::http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok());
    let request = if content_type.is_some_and(|value| {
        value.split(';').next().is_some_and(|media_type| {
            media_type
                .trim()
                .eq_ignore_ascii_case("application/octet-stream")
        })
    }) {
        ActionRequest {
            input: empty_action_input(),
            payload: Some(body),
        }
    } else {
        // Action inputs are versioned JSON contracts.  Do not accept an
        // arbitrary text media type and guess its representation; callers
        // must negotiate the declared JSON or binary action contract.
        if !content_type.is_some_and(|value| {
            value.split(';').next().is_some_and(|media_type| {
                media_type.trim().eq_ignore_ascii_case("application/json")
            })
        }) {
            return ProblemDetails::new(ErrorCode::UnsupportedMediaType).into_response();
        }
        let Ok(input) = serde_json::from_slice::<ActionRequest>(&body) else {
            return ProblemDetails::new(ErrorCode::BadRequest).into_response();
        };
        ActionRequest {
            payload: None,
            ..input
        }
    };
    let key = match idempotency_key(&headers) {
        Ok(Some(key)) => key,
        Ok(None) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    match application
        .action(descriptor, &auth.0, &id, action, request, key)
        .await
    {
        Ok(result) if result.complete => {
            (StatusCode::OK, Json(public_mutation(result))).into_response()
        }
        Ok(result) => (StatusCode::ACCEPTED, Json(public_mutation(result))).into_response(),
        Err(error) => application_problem(error),
    }
}

/// Concrete routes must bind their canonical descriptor explicitly. They do
/// not derive namespace/collection from the request URI.
pub async fn create_compute(
    auth: BearerAuth,
    headers: HeaderMap,
    State(state): State<NativeApiState>,
    Json(request): Json<CreateRequest>,
) -> Response {
    create_for(
        auth,
        headers,
        "compute".to_owned(),
        "servers".to_owned(),
        State(state),
        Json(request),
    )
    .await
}

pub async fn create_volume(
    auth: BearerAuth,
    headers: HeaderMap,
    State(state): State<NativeApiState>,
    Json(request): Json<CreateRequest>,
) -> Response {
    create_for(
        auth,
        headers,
        "volume".to_owned(),
        "volumes".to_owned(),
        State(state),
        Json(request),
    )
    .await
}

async fn create_for(
    auth: BearerAuth,
    headers: HeaderMap,
    namespace: String,
    collection: String,
    State(state): State<NativeApiState>,
    Json(request): Json<CreateRequest>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::with_detail(ErrorCode::ResourceNotFound, "resource type not found")
            .into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::Create) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, None) {
        return ProblemDetails::new(response).into_response();
    }
    if let Err(response) = ready_for_mutation(&state.resource_index, descriptor) {
        return ProblemDetails::new(response).into_response();
    }
    if let Some(kind) = request.kind.as_deref()
        && kind != descriptor.resource_type.to_string()
    {
        return ProblemDetails::with_detail(
            ErrorCode::BadRequest,
            "kind does not match route resource type",
        )
        .into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "resource application is not configured",
        )
        .into_response();
    };
    let resource_type = descriptor.resource_type.to_string();
    let contract = crate::resource_contract::ContractKind::for_resource(
        &resource_type,
        &descriptor.schema_version,
    );
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    let spec = match contract {
        Some(contract) => match contract.validate(request.spec) {
            Ok(spec) => spec,
            Err(_) => {
                return ProblemDetails::with_detail(
                    ErrorCode::BadRequest,
                    "resource spec violates its published contract",
                )
                .into_response();
            }
        },
        // External controllers own their request contracts and validate at
        // their controller boundary; native built-in resources never take
        // this branch.
        None if descriptor.ownership == o3k_kernel::ServiceOwnership::ExternalController => {
            crate::resource_contract::ValidatedSpec::from_external_contract(request.spec)
        }
        None => {
            return ProblemDetails::with_detail(
                ErrorCode::NotAvailable,
                "resource create contract is not available",
            )
            .into_response();
        }
    };
    let validated = ValidatedCreateRequest {
        api_version: request.api_version,
        kind: request.kind,
        spec,
    };
    match application
        .create(descriptor, &auth.0, validated, key)
        .await
    {
        Ok(result) if result.complete => {
            (StatusCode::CREATED, Json(public_mutation(result))).into_response()
        }
        Ok(result) => (StatusCode::ACCEPTED, Json(public_mutation(result))).into_response(),
        Err(error) => application_problem(error),
    }
}

pub async fn delete(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection, id)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    delete_for(auth, headers, namespace, collection, id, State(state)).await
}

/// DELETE handler for concrete native collection routes. Concrete routes
/// retain their specialized GET representations, while mutations use the
/// same manifest-derived application path as the generic route.
pub async fn delete_fixed(
    auth: BearerAuth,
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    delete_for(
        auth,
        headers,
        "compute".to_owned(),
        "servers".to_owned(),
        id,
        State(state),
    )
    .await
}

pub async fn delete_volume(
    auth: BearerAuth,
    headers: HeaderMap,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    delete_for(
        auth,
        headers,
        "volume".to_owned(),
        "volumes".to_owned(),
        id,
        State(state),
    )
    .await
}

async fn delete_for(
    auth: BearerAuth,
    headers: HeaderMap,
    namespace: String,
    collection: String,
    id: String,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::with_detail(ErrorCode::ResourceNotFound, "resource type not found")
            .into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::Delete) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, Some(&id)) {
        return ProblemDetails::new(response).into_response();
    }
    if let Err(response) = ready_for_mutation(&state.resource_index, descriptor) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "resource application is not configured",
        )
        .into_response();
    };
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    // v1 uses one explicit, versioned precondition form for lifecycle
    // mutations: `If-Match: generation-N`.  Ownership is authorized before
    // the application evaluates this value, so a foreign resource cannot
    // disclose its generation.
    let expected_generation = match headers.get("if-match") {
        None => None,
        Some(value) => match value.to_str().ok().and_then(|value| {
            value
                .strip_prefix("generation-")
                .and_then(|generation| generation.parse::<i64>().ok())
        }) {
            Some(generation) if generation >= 0 => Some(generation),
            _ => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
        },
    };
    match application
        .delete(descriptor, &auth.0, &id, key, expected_generation)
        .await
    {
        Ok(result) if result.complete => StatusCode::NO_CONTENT.into_response(),
        Ok(result) => (StatusCode::ACCEPTED, Json(public_mutation(result))).into_response(),
        Err(error) => application_problem(error),
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
    /// Only the stable, indexed ordering vocabulary is accepted.
    #[serde(default = "default_order")]
    pub order: String,
    /// Resource-specific filters must be declared by the resource contract;
    /// arbitrary spec/provider fields are intentionally not queryable.
    #[serde(default, deserialize_with = "deserialize_bounded_filters")]
    pub filter: Vec<String>,
    /// Parsed canonical filter. Only durable observed state is advertised in
    /// v1; adapters must reject every other predicate before touching a store.
    #[serde(skip)]
    pub observed_state: Option<String>,
    #[serde(skip)]
    pub continuation_id: Option<String>,
    /// Parsed bounded page size passed to the repository application. Keeping
    /// this on the request prevents adapters from fetching the global maximum
    /// for every small page request.
    #[serde(skip)]
    pub page_size: usize,
}

fn default_order() -> String {
    "id.asc".to_owned()
}

fn query_hash(query: &ListQuery) -> String {
    use sha2::{Digest, Sha256};
    let canonical = serde_json::to_vec(&(query.order.as_str(), &query.filter)).unwrap_or_default();
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(canonical))
}

// Axum materializes repeated query parameters before the handler runs. Bound
// both their count and size before any adapter/repository work is attempted.
const MAX_LIST_FILTERS: usize = 16;
const MAX_LIST_FILTER_LENGTH: usize = 256;

/// Deserialize repeated `filter` parameters with a hard bound while the
/// query is still being materialized.  Validating `Vec<String>` after the
/// default serde implementation has run would permit an attacker to make the
/// extractor allocate an arbitrarily large vector before returning 400.
fn deserialize_bounded_filters<'de, D>(deserializer: D) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    struct BoundedFilters;

    impl<'de> Visitor<'de> for BoundedFilters {
        type Value = Vec<String>;

        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("at most 16 filter values, each at most 256 bytes long")
        }

        fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            let mut filters = Vec::with_capacity(MAX_LIST_FILTERS);
            while let Some(filter) = seq.next_element::<String>()? {
                if filters.len() == MAX_LIST_FILTERS {
                    return Err(de::Error::custom("too many filter values"));
                }
                if filter.len() > MAX_LIST_FILTER_LENGTH {
                    return Err(de::Error::custom("filter value is too long"));
                }
                filters.push(filter);
            }
            Ok(filters)
        }
    }

    deserializer.deserialize_seq(BoundedFilters)
}

fn validate_filter_shape(filters: &[String]) -> Result<(), ErrorCode> {
    if filters.len() > MAX_LIST_FILTERS
        || filters
            .iter()
            .any(|filter| filter.len() > MAX_LIST_FILTER_LENGTH)
    {
        return Err(ErrorCode::BadRequest);
    }
    Ok(())
}

pub async fn list(
    auth: BearerAuth,
    Path((namespace, collection)): Path<(String, String)>,
    State(state): State<NativeApiState>,
    Query(mut query): Query<ListQuery>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::List) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if !descriptor.ready {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    }
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, None) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if query.order != "id.asc" {
        return ProblemDetails::new(ErrorCode::UnsupportedOperation).into_response();
    }
    if let Err(error) = validate_filter_shape(&query.filter) {
        return ProblemDetails::new(error).into_response();
    }
    for filter in &query.filter {
        let Some(value) = filter.strip_prefix("observed_state=") else {
            return ProblemDetails::new(ErrorCode::UnsupportedOperation).into_response();
        };
        if value.is_empty() || value.len() > 64 || value.chars().any(|c| c.is_control()) {
            return ProblemDetails::new(ErrorCode::BadRequest).into_response();
        }
        if query.observed_state.replace(value.to_owned()).is_some() {
            return ProblemDetails::new(ErrorCode::BadRequest).into_response();
        }
    }
    query.page_size = parse_page_size(query.limit.as_deref());
    let effective_query_hash = query_hash(&query);
    let scope = auth.0.effective_scope().id().to_string();
    let resource_type = descriptor.resource_type.to_string();
    if let Some(cursor) = query.cursor.as_deref() {
        let Ok(payload) = state.cursor_config.decode_cursor(
            cursor,
            &scope,
            &resource_type,
            &effective_query_hash,
        ) else {
            return ProblemDetails::new(ErrorCode::InvalidCursor).into_response();
        };
        match application
            .validate_list_cursor(descriptor, &auth.0, &payload.last_id)
            .await
        {
            Ok(true) => {}
            Ok(false) | Err(_) => {
                return ProblemDetails::new(ErrorCode::InvalidCursor).into_response();
            }
        }
        query.continuation_id = Some(payload.last_id);
    }
    let page_result = match application.list_page(descriptor, &auth.0, &query).await {
        Ok(page) => page,
        Err(error) => return application_problem(error),
    };
    let page_result = page_result.bounded(query.page_size);
    let mut items = page_result
        .items
        .into_iter()
        .map(public_value)
        .collect::<Vec<_>>();
    items.sort_by(|a, b| {
        a["metadata"]["id"]
            .as_str()
            .cmp(&b["metadata"]["id"].as_str())
    });
    let page_size = query.page_size;
    let has_more = page_result.has_more || items.len() > page_size;
    let page = items.into_iter().take(page_size).collect::<Vec<_>>();
    let next_cursor = if has_more {
        page.last()
            .and_then(|item| item["metadata"]["id"].as_str())
            .map(|last_id| {
                state.cursor_config.encode_cursor(&CursorPayload {
                    last_id: last_id.to_owned(),
                    scope_id: scope,
                    resource_type,
                    query_hash: effective_query_hash,
                    version: 1,
                })
            })
    } else {
        None
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": page, "next_cursor": next_cursor})),
    )
        .into_response()
}

pub async fn show(
    auth: BearerAuth,
    Path((namespace, collection, id)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::Show) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if !descriptor.ready {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    }
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, Some(&id)) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    match application.show(descriptor, &auth.0, &id).await {
        Ok(resource) => (StatusCode::OK, Json(public_value(resource))).into_response(),
        Err(error) => application_problem(error),
    }
}

pub async fn relationships(
    auth: BearerAuth,
    Path((namespace, collection, id)): Path<(String, String, String)>,
    Query(query): Query<ListQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(descriptor) = state.resource_index.resolve(&namespace, &collection) else {
        return ProblemDetails::new(ErrorCode::ResourceNotFound).into_response();
    };
    let action = match declared_action(descriptor, LifecycleOperation::Show) {
        Ok(action) => action,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    if let Err(response) = authorize(&state, descriptor, action, &auth.0, Some(&id)) {
        return ProblemDetails::new(response).into_response();
    }
    let Some(application) = state.resource_application else {
        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
    };
    if let Err(error) = validate_filter_shape(&query.filter) {
        return ProblemDetails::new(error).into_response();
    }
    if query.order != "id.asc" || !query.filter.is_empty() {
        return ProblemDetails::new(ErrorCode::UnsupportedOperation).into_response();
    }
    let effective_query_hash = query_hash(&query);
    let scope = auth.0.effective_scope().id().to_string();
    let resource_type = format!("{namespace}:{collection}:relationships:{id}");
    let after_slot = if let Some(cursor) = query.cursor.as_deref() {
        match state.cursor_config.decode_cursor(
            cursor,
            &scope,
            &resource_type,
            &effective_query_hash,
        ) {
            Ok(payload) => Some(payload.last_id),
            Err(_) => return ProblemDetails::new(ErrorCode::InvalidCursor).into_response(),
        }
    } else {
        None
    };
    let page_size = parse_page_size(query.limit.as_deref());
    let mut items = match application
        .relationships_page(
            descriptor,
            &auth.0,
            &id,
            after_slot.as_deref(),
            page_size.saturating_add(1),
        )
        .await
    {
        Ok(items) => items,
        Err(error) => return application_problem(error),
    };
    let has_more = items.len() > page_size;
    if has_more {
        let _ = items.pop();
    }
    let next_cursor = if has_more {
        items.last().map(|item| {
            state.cursor_config.encode_cursor(&CursorPayload {
                last_id: item.slot.clone(),
                scope_id: scope.clone(),
                resource_type: resource_type.clone(),
                query_hash: effective_query_hash.clone(),
                version: 1,
            })
        })
    } else {
        None
    };
    (
        StatusCode::OK,
        Json(serde_json::json!({"items": items, "next_cursor": next_cursor})),
    )
        .into_response()
}

#[cfg(test)]
mod projection_tests {
    use super::public_value;
    use serde_json::json;

    #[test]
    fn public_resource_projection_removes_nested_provider_and_secret_fields() {
        let projected = public_value(json!({
            "metadata": {"id": "r1"},
            "spec": {"name": "safe", "user_data": "payload"},
            "status": {"provider_id": "private-id", "nested": [{"password": "pw"}]},
            "backendId": "backend", "nodeId": "node", "hostPath": "/private",
            "providerReference": "ref", "providerResourceId": "resource",
            "providerOperationId": "operation",
            "connection": "should-not-be-here"
        }));
        assert_eq!(projected["metadata"]["id"], "r1");
        assert!(projected["spec"].get("user_data").is_none());
        assert!(projected["status"].get("provider_id").is_none());
        assert!(projected["status"]["nested"][0].get("password").is_none());
        assert!(projected.get("backendId").is_none());
        assert!(projected.get("nodeId").is_none());
        assert!(projected.get("hostPath").is_none());
        assert!(projected.get("providerReference").is_none());
        assert!(projected.get("providerResourceId").is_none());
        assert!(projected.get("providerOperationId").is_none());
        assert!(projected.get("connection").is_none());
    }
}

#[cfg(test)]
mod bounded_query_tests {
    use super::ListQuery;

    #[test]
    fn repeated_filters_are_rejected_during_deserialization() {
        let query = format!(
            "{{\"filter\":[{}]}}",
            (0..17).map(|_| "\"x\"").collect::<Vec<_>>().join(",")
        );
        assert!(serde_json::from_str::<ListQuery>(&query).is_err());
    }

    #[test]
    fn oversized_filter_is_rejected_during_deserialization() {
        let query = format!(r#"{{"filter":["{}"]}}"#, "x".repeat(257));
        assert!(serde_json::from_str::<ListQuery>(&query).is_err());
    }
}
