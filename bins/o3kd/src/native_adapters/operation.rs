use std::sync::Arc;

use o3k_native_api::error::NativeReadError;
use o3k_store::DurableStore;
use uuid::Uuid;

/// Store-backed canonical operation visibility adapter. Historical operation
/// rows without P12.4 metadata fail closed rather than being reconstructed
/// with fabricated ownership or action fields.
pub struct OperationReaderAdapter {
    pub store: Arc<o3k_store::unified::O3kStore>,
}

#[async_trait::async_trait]
impl o3k_native_api::operation::OperationReader for OperationReaderAdapter {
    async fn show_operation(
        &self,
        auth: &o3k_kernel::AuthContext,
        id: Uuid,
    ) -> Result<o3k_kernel::Operation, NativeReadError> {
        // Establish non-disclosure from the authoritative durable resource
        // owner before touching canonical metadata.  A corrupt foreign row
        // must be indistinguishable from a missing operation.
        let durable = self.store.get_operation(id).await.map_err(|error| {
            if matches!(error, o3k_store::StoreError::OperationNotFound) {
                NativeReadError::NotFound
            } else {
                tracing::error!(%error, operation_id = %id, "native operation owner lookup failed");
                NativeReadError::Internal
            }
        })?;
        let resource_id = durable.resource_id;
        let resource = self.store.get_resource(resource_id).await.map_err(|error| {
            if matches!(error, o3k_store::StoreError::ResourceNotFound) {
                NativeReadError::NotFound
            } else {
                tracing::error!(%error, operation_id = %id, "native operation resource lookup failed");
                NativeReadError::Internal
            }
        })?;
        if resource.project_id != auth.effective_scope().id().as_str()
            || auth.effective_scope().kind() != o3k_kernel::ScopeKind::Project
        {
            return Err(NativeReadError::NotFound);
        }
        let record = self
            .store
            .get_canonical_operation(id)
            .await
            .map_err(|error| {
                if matches!(error, o3k_store::StoreError::OperationNotFound) {
                    NativeReadError::NotFound
                } else {
                    tracing::error!(%error, operation_id = %id, "native operation read failed");
                    NativeReadError::Internal
                }
            })?;
        let operation = o3k_kernel::Operation::try_from(record).map_err(|error| {
            tracing::error!(%error, operation_id = %id, "invalid canonical operation metadata");
            NativeReadError::Internal
        })?;
        if operation.owner_scope.kind() != auth.effective_scope().kind()
            || operation.owner_scope.id() != auth.effective_scope().id()
        {
            return Err(NativeReadError::NotFound);
        }
        Ok(operation)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod operation_visibility_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use o3k_kernel::{AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, UserPrincipal};
    use o3k_native_api::auth::{NativeTokenRequestV1, TokenIssuer};
    use o3k_native_api::error::ProblemDetails;
    use o3k_store::{
        CanonicalOperationRecord, DurableStore, IdempotencyReservationRequest, OperationRecord,
        OperationState, ResourceRecord,
    };
    use std::path::PathBuf;
    use std::sync::Arc;
    use tower::util::ServiceExt;
    use uuid::Uuid;

    struct TestIssuer;

    fn context(project: &str) -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked(format!("user-{project}")),
                format!("user-{project}"),
                None,
            )),
            OwnershipScope::project(ScopeId::new_unchecked(project), None, None),
            vec!["member".into()],
            1,
            u64::MAX,
            "audit",
            "request",
            None,
        )
    }

    #[async_trait::async_trait]
    impl TokenIssuer for TestIssuer {
        async fn issue_native(
            &self,
            _request: &NativeTokenRequestV1,
        ) -> Result<(String, serde_json::Value), ProblemDetails> {
            Err(ProblemDetails::bad_request(
                "test issuer does not issue tokens",
            ))
        }

        async fn auth_context(&self, token: &str) -> Result<AuthContext, ProblemDetails> {
            token
                .strip_prefix("project-")
                .map(|project| context(&format!("project-{project}")))
                .ok_or_else(ProblemDetails::unauthorized)
        }
    }

    fn temp_db() -> PathBuf {
        std::env::temp_dir().join(format!("o3k-p12-4-op-api-{}.db", Uuid::new_v4()))
    }

    async fn seed() -> (Arc<o3k_store::unified::O3kStore>, Uuid, PathBuf) {
        let path = temp_db();
        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_file(&path)
                .await
                .expect("sqlite store"),
        );
        let id = Uuid::new_v4();
        let resource_id = Uuid::new_v4();
        store
            .insert_resource(&ResourceRecord {
                id: resource_id,
                kind: "compute:server".into(),
                project_id: "project-a".into(),
                generation: 1,
                observed_generation: 1,
                desired_state: "active".into(),
                observed_state: "active".into(),
                provider_id: Some("secret-provider-resource".into()),
            })
            .await
            .expect("resource");
        let durable = OperationRecord {
            id,
            resource_id,
            kind: "native:create".into(),
            state: OperationState::Succeeded,
            provider_operation_id: Some("secret-provider-op".into()),
            error_category: None,
            error_message: Some("secret provider detail".into()),
        };
        let canonical = CanonicalOperationRecord {
            id,
            service: "compute".into(),
            action: "compute:CreateServer".into(),
            actor: "user-project-a".into(),
            owner_scope: "project-a".into(),
            resource_type: "compute:server".into(),
            resource_id: Some(resource_id.to_string()),
            state: OperationState::Succeeded,
            attempt: 1,
            created_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: Some("2026-01-01T00:00:01Z".into()),
            error: None,
            request_id: Some("req-a".into()),
        };
        let reservation = IdempotencyReservationRequest::from_semantics(
            "project-a",
            "compute:CreateServer",
            "native-operation-visibility",
            "compute:server",
            Some(&resource_id.to_string()),
            &serde_json::json!({"resource_id": resource_id}),
            id,
        )
        .expect("idempotency identity");
        store
            .create_or_replay_canonical_idempotent_operation(&durable, &canonical, &reservation)
            .await
            .expect("canonical operation triplet");
        // Exercise the same durable reconstruction path used after an o3kd
        // restart, rather than serving the record from the seeding pool.
        drop(store);
        let reopened = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_file(&path)
                .await
                .expect("reopen sqlite store"),
        );
        (reopened, id, path)
    }

    #[tokio::test]
    async fn operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields() {
        let (store, id, path) = seed().await;
        let reader = Arc::new(OperationReaderAdapter { store });
        let native = o3k_native_api::NativeApiState::new(
            None,
            o3k_native_api::pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer)),
            None,
            None,
            None,
        )
        .expect("test manifest registry is valid")
        .with_operation_reader(reader);
        let app = o3k_api::router_with_state(o3k_api::AppState::new().with_native_api(native));

        let request = |project: &str, operation: Uuid| {
            Request::builder()
                .uri(format!("/o3k/v1/operations/{operation}"))
                .header("authorization", format!("Bearer project-{project}"))
                .body(Body::empty())
                .expect("request")
        };
        let owner = app
            .clone()
            .oneshot(request("a", id))
            .await
            .expect("owner response");
        assert_eq!(owner.status(), StatusCode::OK);
        let body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(owner.into_body(), usize::MAX)
                .await
                .expect("body"),
        )
        .expect("json");
        assert_eq!(body["id"], id.to_string());
        assert_eq!(body["owner_scope"]["id"], "project-a");
        let serialized = body.to_string();
        for forbidden in [
            "provider_operation_id",
            "provider_resource_id",
            "secret-provider-op",
            "secret-provider-resource",
            "secret provider detail",
            "agent_id",
            "agent_epoch",
            "database",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "native operation response leaked `{forbidden}`"
            );
        }

        let foreign = app
            .clone()
            .oneshot(request("b", id))
            .await
            .expect("foreign response");
        let missing = app
            .oneshot(request("b", Uuid::new_v4()))
            .await
            .expect("missing response");
        assert_eq!(foreign.status(), StatusCode::NOT_FOUND);
        assert_eq!(missing.status(), StatusCode::NOT_FOUND);
        let foreign_body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(foreign.into_body(), usize::MAX)
                .await
                .expect("foreign body"),
        )
        .expect("foreign json");
        let missing_body: serde_json::Value = serde_json::from_slice(
            &axum::body::to_bytes(missing.into_body(), usize::MAX)
                .await
                .expect("missing body"),
        )
        .expect("missing json");
        assert_eq!(foreign_body["status"], missing_body["status"]);
        assert_eq!(foreign_body["code"], missing_body["code"]);
        assert_eq!(foreign_body["title"], missing_body["title"]);
        std::fs::remove_file(path).expect("cleanup");
    }
}
