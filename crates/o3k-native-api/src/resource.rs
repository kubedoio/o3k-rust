//! Generic native resource application boundary.
//!
//! This module deliberately contains no provider or controller types.  Native
//! adapters resolve a validated descriptor and hand the request to this port;
//! the implementation below the port owns canonical resources, operations and
//! idempotency.
#![allow(clippy::items_after_test_module)]

use std::{collections::HashMap, sync::Arc};

use crate::pagination::{CursorPayload, continuation_index, parse_page_size};
use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};
use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuthContext, AuthorizationDecision, AuthorizationRequest, ResourceTarget,
    ResourceType,
};
use serde::{Deserialize, Serialize};

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
}

impl ResourceDispatcher {
    /// Builds the dispatch index from the canonical manifest registry. This
    /// index is derived state and is never independently registered by API
    /// callers.
    pub fn from_manifest_registry(
        manifests: &o3k_kernel::ManifestRegistry,
    ) -> Result<Self, DescriptorError> {
        let mut index = Self::default();
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
                    ready,
                })?;
            }
        }
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
    pub fn all(&self) -> impl Iterator<Item = &ResourceDescriptor> {
        self.descriptors.values()
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

#[async_trait]
pub trait ResourceApplication: Send + Sync {
    async fn create(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        request: CreateRequest,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError>;
    async fn delete(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, ResourceApplicationError>;
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
    ) -> Result<Vec<serde_json::Value>, ResourceApplicationError>;
    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, ResourceApplicationError>;
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

fn ready_for_mutation(descriptor: &ResourceDescriptor) -> Result<(), ErrorCode> {
    if descriptor.ready {
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
}

pub async fn create(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection)): Path<(String, String)>,
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
    if let Err(response) = ready_for_mutation(descriptor) {
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
    let key = match idempotency_key(&headers) {
        Ok(key) => key,
        Err(error) => return ProblemDetails::new(error).into_response(),
    };
    match application.create(descriptor, &auth.0, request, key).await {
        Ok(result) if result.complete => (StatusCode::CREATED, Json(result)).into_response(),
        Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(error) => application_problem(error),
    }
}

pub async fn delete(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection, id)): Path<(String, String, String)>,
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
    if let Err(response) = ready_for_mutation(descriptor) {
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
    match application.delete(descriptor, &auth.0, &id, key).await {
        Ok(result) if result.complete => StatusCode::NO_CONTENT.into_response(),
        Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(error) => application_problem(error),
    }
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

pub async fn list(
    auth: BearerAuth,
    Path((namespace, collection)): Path<(String, String)>,
    State(state): State<NativeApiState>,
    Query(query): Query<ListQuery>,
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
    let items = match application.list(descriptor, &auth.0).await {
        Ok(items) => items,
        Err(error) => return application_problem(error),
    };
    let scope = auth.0.effective_scope().id().to_string();
    let resource_type = descriptor.resource_type.to_string();
    let mut items = items;
    items.sort_by(|a, b| {
        a["metadata"]["id"]
            .as_str()
            .cmp(&b["metadata"]["id"].as_str())
    });
    let start = if let Some(cursor) = query.cursor.as_deref() {
        let Ok(payload) = state
            .cursor_config
            .decode_cursor(cursor, &scope, &resource_type)
        else {
            return ProblemDetails::new(ErrorCode::InvalidCursor).into_response();
        };
        let ids = items
            .iter()
            .filter_map(|item| item["metadata"]["id"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        match continuation_index(&ids, &payload.last_id) {
            Ok(index) => index,
            Err(_) => return ProblemDetails::new(ErrorCode::InvalidCursor).into_response(),
        }
    } else {
        0
    };
    let page_size = parse_page_size(query.limit.as_deref());
    let end = (start + page_size).min(items.len());
    let page = items[start..end].to_vec();
    let next_cursor = if end < items.len() {
        page.last()
            .and_then(|item| item["metadata"]["id"].as_str())
            .map(|last_id| {
                state.cursor_config.encode_cursor(&CursorPayload {
                    last_id: last_id.to_owned(),
                    scope_id: scope,
                    resource_type,
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
        Ok(resource) => (StatusCode::OK, Json(resource)).into_response(),
        Err(error) => application_problem(error),
    }
}
