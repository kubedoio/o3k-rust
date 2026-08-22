//! Generic native resource application boundary.
//!
//! This module deliberately contains no provider or controller types.  Native
//! adapters resolve a validated descriptor and hand the request to this port;
//! the implementation below the port owns canonical resources, operations and
//! idempotency.
#![allow(clippy::items_after_test_module)]

use std::{collections::HashMap, sync::Arc};

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
};
use async_trait::async_trait;
use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use o3k_kernel::{ActionId, AuthContext, ResourceType};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleOperation {
    Create,
    Delete,
    List,
    Show,
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
}

#[derive(Debug, Clone, Default)]
pub struct ResourceRegistry {
    descriptors: HashMap<(String, String), ResourceDescriptor>,
}

impl ResourceRegistry {
    pub fn register(&mut self, descriptor: ResourceDescriptor) -> Result<(), DescriptorError> {
        if descriptor.collection.trim().is_empty() {
            return Err(DescriptorError::EmptyCollection);
        }
        if ["services", "resource-types", "identity", "operations"]
            .contains(&descriptor.collection.as_str())
        {
            return Err(DescriptorError::ReservedCollection);
        }
        for op in [
            LifecycleOperation::Create,
            LifecycleOperation::Delete,
            LifecycleOperation::List,
            LifecycleOperation::Show,
        ] {
            if !descriptor.lifecycle_actions.contains_key(&op) {
                return Err(DescriptorError::MissingAction(op));
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
pub struct CreateRequest {
    pub api_version: Option<String>,
    pub kind: Option<String>,
    #[serde(default)]
    pub spec: serde_json::Value,
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
    ) -> Result<MutationResult, String>;
    async fn delete(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
        idempotency_key: Option<&str>,
    ) -> Result<MutationResult, String>;
    async fn list(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
    ) -> Result<Vec<serde_json::Value>, String>;
    async fn show(
        &self,
        descriptor: &ResourceDescriptor,
        auth: &AuthContext,
        id: &str,
    ) -> Result<serde_json::Value, String>;
}

pub type SharedResourceApplication = Arc<dyn ResourceApplication>;

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
        let mut registry = ResourceRegistry::default();
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
        let mut registry = ResourceRegistry::default();
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
    fn registry_requires_explicit_lifecycle_actions() {
        let mut registry = ResourceRegistry::default();
        let mut d = descriptor("compute", "server", "servers");
        d.lifecycle_actions.remove(&LifecycleOperation::Delete);
        assert_eq!(
            registry.register(d),
            Err(DescriptorError::MissingAction(LifecycleOperation::Delete))
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
    let Some(descriptor) = state.resource_registry.resolve(&namespace, &collection) else {
        return ProblemDetails::with_detail(ErrorCode::ResourceNotFound, "resource type not found")
            .into_response();
    };
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
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    match application.create(descriptor, &auth.0, request, key).await {
        Ok(result) if result.complete => (StatusCode::CREATED, Json(result)).into_response(),
        Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(_) => {
            ProblemDetails::with_detail(ErrorCode::InternalError, "resource application failed")
                .into_response()
        }
    }
}

pub async fn delete(
    auth: BearerAuth,
    headers: HeaderMap,
    Path((namespace, collection, id)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    let Some(descriptor) = state.resource_registry.resolve(&namespace, &collection) else {
        return ProblemDetails::with_detail(ErrorCode::ResourceNotFound, "resource type not found")
            .into_response();
    };
    let Some(application) = state.resource_application else {
        return ProblemDetails::with_detail(
            ErrorCode::NotAvailable,
            "resource application is not configured",
        )
        .into_response();
    };
    let key = headers.get("idempotency-key").and_then(|v| v.to_str().ok());
    match application.delete(descriptor, &auth.0, &id, key).await {
        Ok(result) if result.complete => StatusCode::NO_CONTENT.into_response(),
        Ok(result) => (StatusCode::ACCEPTED, Json(result)).into_response(),
        Err(_) => {
            ProblemDetails::with_detail(ErrorCode::InternalError, "resource application failed")
                .into_response()
        }
    }
}
