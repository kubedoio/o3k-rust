#![allow(clippy::expect_used)]
//! Regression: the production composition router must expose the SPEC-0030
//! generic lifecycle update on the canonical compute collection. The generic
//! `PUT /o3k/v1/{namespace}/{collection}/{id}` route cannot serve
//! `/o3k/v1/compute/servers/{id}` because axum prioritizes the static route,
//! so the static binding must carry `.put(...)` itself, and the production
//! `seed_core` manifest must declare the `update` lifecycle operation — a
//! manifest without it fails closed with UnsupportedOperation.

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use o3k_kernel::{
    AuthContext, ManifestRegistry, OwnershipScope, Principal, PrincipalId, ScopeId, ScopeKind,
    UserPrincipal,
};
use serde_json::{Value, json};
use std::sync::Arc;
use tower::ServiceExt;

#[derive(Clone)]
struct TestIssuer(AuthContext);

#[async_trait::async_trait]
impl o3k_native_api::auth::TokenIssuer for TestIssuer {
    async fn issue_native(
        &self,
        _request: &o3k_native_api::auth::NativeTokenRequestV1,
    ) -> Result<(String, Value), o3k_native_api::error::ProblemDetails> {
        Err(o3k_native_api::error::ProblemDetails::unauthorized())
    }

    async fn auth_context(
        &self,
        _token: &str,
    ) -> Result<AuthContext, o3k_native_api::error::ProblemDetails> {
        Ok(self.0.clone())
    }
}

/// Application stub whose only job is to prove the request reaches the
/// canonical generic update application through the production router.
#[derive(Clone)]
struct UpdateRecordingApplication;

#[async_trait::async_trait]
impl o3k_native_api::resource::ResourceApplication for UpdateRecordingApplication {
    async fn create(
        &self,
        _descriptor: &o3k_native_api::resource::ResourceDescriptor,
        _auth: &AuthContext,
        _request: o3k_native_api::resource::ValidatedCreateRequest,
        _idempotency_key: Option<&str>,
    ) -> Result<
        o3k_native_api::resource::MutationResult,
        o3k_native_api::resource::ResourceApplicationError,
    > {
        Err(o3k_native_api::resource::ResourceApplicationError::UnsupportedOperation)
    }

    async fn delete(
        &self,
        _descriptor: &o3k_native_api::resource::ResourceDescriptor,
        _auth: &AuthContext,
        _id: &str,
        _idempotency_key: Option<&str>,
        _expected_generation: Option<i64>,
    ) -> Result<
        o3k_native_api::resource::MutationResult,
        o3k_native_api::resource::ResourceApplicationError,
    > {
        Err(o3k_native_api::resource::ResourceApplicationError::UnsupportedOperation)
    }

    async fn update(
        &self,
        descriptor: &o3k_native_api::resource::ResourceDescriptor,
        _auth: &AuthContext,
        id: &str,
        _request: o3k_native_api::resource::ValidatedUpdateRequest,
        idempotency_key: Option<&str>,
        expected_generation: i64,
    ) -> Result<
        o3k_native_api::resource::MutationResult,
        o3k_native_api::resource::ResourceApplicationError,
    > {
        assert_eq!(descriptor.resource_type.to_string(), "compute:server");
        assert_eq!(idempotency_key, Some("update-key"));
        assert_eq!(expected_generation, 1);
        Ok(o3k_native_api::resource::MutationResult {
            operation_id: format!("update-op-{id}"),
            resource_id: Some(id.to_owned()),
            complete: true,
            resource: None,
        })
    }

    async fn list_page(
        &self,
        _descriptor: &o3k_native_api::resource::ResourceDescriptor,
        _auth: &AuthContext,
        _query: &o3k_native_api::pagination::ResourceQuery,
        _cursors: &o3k_native_api::pagination::CursorConfig,
    ) -> Result<
        o3k_native_api::pagination::ResourcePage<Value>,
        o3k_native_api::resource::ResourceApplicationError,
    > {
        Err(o3k_native_api::resource::ResourceApplicationError::UnsupportedOperation)
    }

    async fn show(
        &self,
        _descriptor: &o3k_native_api::resource::ResourceDescriptor,
        _auth: &AuthContext,
        _id: &str,
    ) -> Result<Value, o3k_native_api::resource::ResourceApplicationError> {
        Err(o3k_native_api::resource::ResourceApplicationError::UnsupportedOperation)
    }
}

fn production_registry() -> ManifestRegistry {
    let mut registry = ManifestRegistry::new();
    registry.seed_core().expect("seed core");
    registry
        .register_controller(
            "compute",
            o3k_kernel::controller::ControllerSession {
                service_id: "compute".to_owned(),
                namespace: "compute".to_owned(),
                service_principal: o3k_kernel::ServicePrincipal::new(
                    PrincipalId::new_unchecked("test-controller"),
                    "test-controller",
                    "compute",
                ),
                session_id: uuid::Uuid::new_v4(),
                session_generation: 1,
                protocol_version: o3k_kernel::controller::ProtocolVersion::new(1, 0),
                manifest_digest: "test-digest".to_owned(),
                manifest_generation: 1,
                started_at: "2026-01-01T00:00:00Z".to_owned(),
            },
        )
        .expect("register compute controller");
    registry.activate_controller("compute").expect("activate");
    registry
}

fn tenant_context() -> AuthContext {
    AuthContext::new(
        Principal::User(UserPrincipal::new(
            PrincipalId::new_unchecked("user-a"),
            "alice",
            None,
        )),
        OwnershipScope::new(
            ScopeId::new_unchecked("project-a"),
            ScopeKind::Project,
            None,
            None,
        ),
        vec!["member".to_owned()],
        1,
        2,
        "audit-test",
        "request-test",
        None,
    )
}

fn production_native_router() -> Result<axum::Router, String> {
    let native = o3k_native_api::NativeApiState::new(
        Some(production_registry()),
        o3k_native_api::pagination::CursorConfig::default(),
        Some(Arc::new(TestIssuer(tenant_context()))),
        None,
        None,
        None,
    )?
    .with_resource_application(Arc::new(UpdateRecordingApplication))
    .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
    Ok(o3k_api::router_with_state(
        o3k_api::AppState::new().with_native_api(native),
    ))
}

#[tokio::test]
async fn production_router_compute_update_reaches_generic_update_application()
-> Result<(), Box<dyn std::error::Error>> {
    let router = production_native_router()?;
    let server_id = uuid::Uuid::new_v4();
    let response = router
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(format!("/o3k/v1/compute/servers/{server_id}"))
                .header("authorization", "Bearer tenant-token")
                .header("content-type", "application/json")
                .header("idempotency-key", "update-key")
                .header("if-match", "generation-1")
                .body(Body::from(json!({"spec": {"name": "renamed"}}).to_string()))?,
        )
        .await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 64 * 1024).await?;
    assert_eq!(
        status,
        StatusCode::OK,
        "{}",
        String::from_utf8_lossy(&bytes)
    );
    let body: Value = serde_json::from_slice(&bytes)?;
    assert_eq!(
        body["operation_id"],
        Value::from(format!("update-op-{server_id}"))
    );
    Ok(())
}

#[tokio::test]
async fn production_discovery_advertises_compute_update_lifecycle_operation()
-> Result<(), Box<dyn std::error::Error>> {
    let router = production_native_router()?;
    let response = router
        .oneshot(
            Request::builder()
                .uri("/o3k/v1/resource-types")
                .body(Body::empty())?,
        )
        .await?;
    assert_eq!(response.status(), StatusCode::OK);
    let body: Value = serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await?)?;
    let server = body["resource_types"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["namespace"] == "compute" && item["name"] == "server")
        })
        .expect("compute:server advertised");
    assert_eq!(
        server["lifecycle_actions"]["update"],
        Value::from("compute:UpdateServer"),
        "{server}"
    );
    Ok(())
}
