use super::*;
use std::collections::BTreeMap;

use o3k_native_api::error::ProblemDetails;

/// Store-backed canonical operation visibility adapter. Historical operation
/// rows without P12.4 metadata fail closed rather than being reconstructed
/// with fabricated ownership or action fields.
#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
mod native_compute_tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use o3k_kernel::{
        ActionId, AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, UserPrincipal,
    };
    use o3k_native_api::auth::{NativeTokenRequestV1, TokenIssuer};
    use o3k_provider::{FailureInjection, FakeComputeProvider};
    use o3k_store::DurableStore;
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

    fn compute_manifest_registry() -> o3k_kernel::ManifestRegistry {
        use std::collections::HashMap;
        let mut reg = o3k_kernel::ManifestRegistry::new();
        let mut ops = HashMap::new();
        ops.insert(
            "create".to_owned(),
            ActionId::new_unchecked("compute", "CreateServer"),
        );
        ops.insert(
            "delete".to_owned(),
            ActionId::new_unchecked("compute", "DeleteServer"),
        );
        ops.insert(
            "list".to_owned(),
            ActionId::new_unchecked("compute", "ListServers"),
        );
        ops.insert(
            "show".to_owned(),
            ActionId::new_unchecked("compute", "ShowServer"),
        );
        let m = o3k_kernel::ServiceManifest {
            manifest_version: 1,
            service_id: "compute".to_owned(),
            namespace: "compute".to_owned(),
            service_version: "0.4.0".to_owned(),
            ownership: o3k_kernel::ServiceOwnership::O3kImplemented,
            resource_types: vec![o3k_kernel::RegisteredResourceType {
                resource_type: o3k_kernel::ResourceType::new_unchecked("compute", "server"),
                schema_version: "v1".to_owned(),
                collection: Some("servers".to_owned()),
                scope: o3k_kernel::ResourceScope::Tenant,
                operations: ops,
            }],
            actions: vec![
                "compute:ListServers".to_owned(),
                "compute:CreateServer".to_owned(),
                "compute:DeleteServer".to_owned(),
                "compute:ShowServer".to_owned(),
            ],
            capabilities: vec![],
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
        };
        let _ = reg.register(m);
        let _ = reg.register_controller(
            "compute",
            o3k_kernel::controller::ControllerSession {
                service_id: "compute".to_owned(),
                namespace: "compute".to_owned(),
                service_principal: o3k_kernel::ServicePrincipal::new(
                    o3k_kernel::PrincipalId::new_unchecked("test-controller"),
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
        );
        let _ = reg.activate_controller("compute");
        reg
    }

    async fn setup() -> (
        axum::Router,
        Arc<o3k_store::unified::O3kStore>,
        Arc<FakeComputeProvider>,
        Uuid,
    ) {
        use axum::routing::get;
        use axum::{Router, extract::DefaultBodyLimit};
        use o3k_native_api::{operation, resource};

        let store = Arc::new(
            o3k_store::unified::O3kStore::connect_sqlite_memory()
                .await
                .expect("store"),
        );
        let provider = Arc::new(FakeComputeProvider::new());
        let compute = Arc::new(o3k_compute::ComputeService::new(
            store.clone(),
            provider.clone(),
        ));
        let network_service = Arc::new(
            o3k_network::NetworkService::open(
                std::env::temp_dir().join(format!("o3k-native-test-{}", Uuid::new_v4())),
                store.clone(),
            )
            .await
            .expect("network service"),
        );
        let foreign_network = network_service
            .create_network_for_project("project-b", "foreign-network".to_owned())
            .await
            .expect("foreign network");
        network_service
            .create_subnet_for_project(
                "project-b",
                foreign_network.id,
                "foreign-subnet".to_owned(),
                "198.51.100.0/24".to_owned(),
                None,
                Some("198.51.100.10".parse().expect("pool start")),
                Some("198.51.100.200".parse().expect("pool end")),
            )
            .await
            .expect("foreign subnet");
        let foreign_port = network_service
            .create_port_for_project("project-b", foreign_network.id, "foreign-port".to_owned())
            .await
            .expect("foreign port")
            .id;

        let app = GenericResourceApplication {
            compute: compute.clone(),
            network_service,
            store: store.clone(),
            storage_provider: None,
            server: Arc::new(ServerReaderAdapter {
                service: compute.clone(),
            }),
            network: Arc::new(NetworkReaderAdapter {
                store: store.clone(),
                authorizer: Arc::new(o3k_kernel::StaticAuthorizer::empty()),
            }),
            external_controllers: Arc::new(BTreeMap::new()),
        };

        let native = o3k_native_api::NativeApiState::new(
            Some(compute_manifest_registry()),
            o3k_native_api::pagination::CursorConfig::default(),
            Some(Arc::new(TestIssuer)),
            Some(Arc::new(ServerReaderAdapter {
                service: compute.clone(),
            })),
            None,
            None,
        )
        .expect("native state")
        .with_operation_reader(Arc::new(OperationReaderAdapter {
            store: store.clone(),
        }))
        .with_resource_application(Arc::new(app))
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));

        // Build a minimal router that only has generic resource routes and
        // the operation route — we deliberately omit the concrete
        // /compute/servers GET-only route so that POST to the generic
        // {namespace}/{collection} route resolves correctly.
        let router = Router::new()
            .route(
                "/{namespace}/{collection}",
                get(resource::list).post(resource::create),
            )
            .route(
                "/{namespace}/{collection}/{id}",
                get(resource::show).delete(resource::delete),
            )
            .route("/operations/{id}", get(operation::show_operation))
            .layer(DefaultBodyLimit::max(1_048_576))
            .with_state(native);

        (router, store, provider, foreign_port)
    }

    fn authed(path: &str, project: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .header("authorization", format!("Bearer project-{project}"))
            .body(Body::empty())
            .expect("request")
    }

    fn authed_post(
        path: &str,
        project: &str,
        idempotency_key: &str,
        body: serde_json::Value,
    ) -> Request<Body> {
        Request::builder()
            .uri(path)
            .method("POST")
            .header("authorization", format!("Bearer project-{project}"))
            .header("content-type", "application/json")
            .header("idempotency-key", idempotency_key)
            .body(Body::from(serde_json::to_vec(&body).unwrap()))
            .expect("request")
    }

    fn authed_delete(path: &str, project: &str, idempotency_key: &str) -> Request<Body> {
        Request::builder()
            .uri(path)
            .method("DELETE")
            .header("authorization", format!("Bearer project-{project}"))
            .header("idempotency-key", idempotency_key)
            .body(Body::empty())
            .expect("request")
    }

    /// Helper: send a request through a cloned router and return (status, parsed JSON body).
    /// Panics if the body is not valid JSON.
    async fn exec(router: &axum::Router, req: Request<Body>) -> (StatusCode, serde_json::Value) {
        let response = router.clone().oneshot(req).await.expect("request");
        let status = response.status();
        let body_bytes = &axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let json: serde_json::Value = serde_json::from_slice(body_bytes).expect("json");
        (status, json)
    }

    // ── Tests ───────────────────────────────────────────────────────────────

    #[tokio::test]
    async fn native_compute_create_and_read_operation() {
        let (router, _, _, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // POST create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(json["complete"].as_bool().unwrap());
        let operation_id = json["operation_id"].as_str().unwrap().to_owned();
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // GET /operations/{id}
        let (status, op) = exec(router, authed(&format!("/operations/{operation_id}"), "a")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(op["id"], operation_id);
        assert_eq!(op["action"]["namespace"], "compute");
        assert_eq!(op["action"]["action"], "CreateServer");
        assert_eq!(op["resource_type"]["namespace"], "compute");
        assert_eq!(op["resource_type"]["name"], "server");
        assert_eq!(op["resource_id"], resource_id);
        assert_eq!(op["owner_scope"]["id"], "project-a");
    }

    #[tokio::test]
    async fn native_compute_create_replay_equivalent() {
        let (router, _, provider, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // First create
        let (status, first) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert!(first["complete"].as_bool().unwrap());

        // Replay with same key
        let (status, replay) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(replay["operation_id"], first["operation_id"]);
        assert_eq!(replay["resource_id"], first["resource_id"]);
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_compute_create_changed_body_conflict() {
        let (router, _, provider, _) = setup().await;
        let router = &router;
        let body_a = serde_json::json!({
            "spec": {
                "name": "test-a",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });
        let body_b = serde_json::json!({
            "spec": {
                "name": "test-b",
                "image_id": "image-b",
                "flavor_id": "00000000-0000-0000-0000-000000000002",
                "network_ids": ["net-b"]
            }
        });

        // First create
        let (status, _) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body_a),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // Replay with DIFFERENT body → 409 Conflict
        let (status, _) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body_b),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_compute_idempotency_isolated_between_owner_scopes() {
        let (router, store, provider, _) = setup().await;
        let body_a = serde_json::json!({"spec":{"name":"tenant-a","image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}});
        let body_b = serde_json::json!({"spec":{"name":"tenant-b","image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}});

        let (status_a, first_a) = exec(
            &router,
            authed_post("/compute/servers", "a", "shared-key", body_a.clone()),
        )
        .await;
        assert_eq!(status_a, StatusCode::CREATED);
        let (_, replay_a) = exec(
            &router,
            authed_post("/compute/servers", "a", "shared-key", body_a),
        )
        .await;
        assert_eq!(replay_a["resource_id"], first_a["resource_id"]);
        assert_eq!(replay_a["operation_id"], first_a["operation_id"]);

        let conflict_a = exec(&router, authed_post("/compute/servers", "a", "shared-key", serde_json::json!({"spec":{"name":"other-a","image_id":"image-b","flavor_id":"00000000-0000-0000-0000-000000000002","network_ids":["net-b"]}}))).await;
        assert_eq!(conflict_a.0, StatusCode::CONFLICT);

        let (status_b, first_b) = exec(
            &router,
            authed_post("/compute/servers", "b", "shared-key", body_b.clone()),
        )
        .await;
        assert_eq!(status_b, StatusCode::CREATED, "tenant B create: {first_b}");
        assert_ne!(first_b["resource_id"], first_a["resource_id"]);
        assert_ne!(first_b["operation_id"], first_a["operation_id"]);
        let b_resource = store
            .get_resource(uuid::Uuid::parse_str(first_b["resource_id"].as_str().unwrap()).unwrap())
            .await
            .expect("tenant B resource");
        assert_eq!(b_resource.project_id, "project-b");

        let (_, replay_b) = exec(
            &router,
            authed_post("/compute/servers", "b", "shared-key", body_b),
        )
        .await;
        assert_eq!(replay_b["resource_id"], first_b["resource_id"]);
        assert_eq!(replay_b["operation_id"], first_b["operation_id"]);
        let conflict_b = exec(&router, authed_post("/compute/servers", "b", "shared-key", serde_json::json!({"spec":{"name":"other-b","image_id":"image-b","flavor_id":"00000000-0000-0000-0000-000000000002","network_ids":["net-b"]}}))).await;
        assert_eq!(conflict_b.0, StatusCode::CONFLICT);

        assert_eq!(
            exec(
                &router,
                authed(
                    &format!(
                        "/compute/servers/{}",
                        first_a["resource_id"].as_str().unwrap()
                    ),
                    "b"
                )
            )
            .await
            .0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            exec(
                &router,
                authed(
                    &format!("/operations/{}", first_a["operation_id"].as_str().unwrap()),
                    "b"
                )
            )
            .await
            .0,
            StatusCode::NOT_FOUND
        );
        assert_eq!(provider.instance_count(), 2);
    }

    #[tokio::test]
    async fn native_http_cursor_is_bound_to_owner_and_rejects_tampering() {
        let (router, _, provider, _) = setup().await;
        for (key, name) in [("page-a", "page-a"), ("page-b", "page-b")] {
            let (status, _) = exec(
                &router,
                authed_post(
                    "/compute/servers",
                    "a",
                    key,
                    serde_json::json!({"spec":{"name":name,"image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let (_, page_a) = exec(&router, authed("/compute/servers?limit=1", "a")).await;
        let cursor_a = page_a["next_cursor"].as_str().expect("tenant A cursor");
        let (second_status, second_page) = exec(
            &router,
            authed(&format!("/compute/servers?limit=1&cursor={cursor_a}"), "a"),
        )
        .await;
        assert_eq!(second_status, StatusCode::OK);
        assert_eq!(second_page["items"].as_array().map(Vec::len), Some(1));
        let (cross_scope_status, _) = exec(
            &router,
            authed(&format!("/compute/servers?limit=1&cursor={cursor_a}"), "b"),
        )
        .await;
        assert_eq!(cross_scope_status, StatusCode::BAD_REQUEST);
        let (tampered_status, _) = exec(
            &router,
            authed(&format!("/compute/servers?limit=1&cursor={cursor_a}x"), "a"),
        )
        .await;
        assert_eq!(tampered_status, StatusCode::BAD_REQUEST);
        assert_eq!(provider.instance_count(), 2);
    }

    #[tokio::test]
    async fn native_http_cursor_continues_deterministically_after_anchor_deletion() {
        let (router, _, provider, _) = setup().await;
        for (key, name) in [("stale-a", "stale-a"), ("stale-b", "stale-b")] {
            let (status, _) = exec(
                &router,
                authed_post(
                    "/compute/servers",
                    "a",
                    key,
                    serde_json::json!({"spec":{"name":name,"image_id":"image-a","flavor_id":"00000000-0000-0000-0000-000000000001","network_ids":["net-a"]}}),
                ),
            )
            .await;
            assert_eq!(status, StatusCode::CREATED);
        }
        let (_, first_page) = exec(&router, authed("/compute/servers?limit=1", "a")).await;
        let anchor_id = first_page["items"][0]["metadata"]["id"]
            .as_str()
            .expect("cursor anchor")
            .to_owned();
        let cursor = first_page["next_cursor"].as_str().expect("cursor");
        let delete_response = router
            .clone()
            .oneshot(authed_delete(
                &format!("/compute/servers/{anchor_id}"),
                "a",
                "stale-delete",
            ))
            .await
            .expect("delete response");
        assert_eq!(delete_response.status(), StatusCode::NO_CONTENT);
        let (status, page_after_delete) = exec(
            &router,
            authed(&format!("/compute/servers?limit=1&cursor={cursor}"), "a"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page_after_delete.get("items").is_none());
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_http_oversized_body_is_rejected_before_provider_mutation() {
        let (router, _, provider, _) = setup().await;
        let mut body = vec![b' '; 1_048_577];
        body[0] = b'{';
        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Bearer project-a")
                    .header("content-type", "application/json")
                    .header("idempotency-key", "oversized")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(provider.instance_count(), 0);
    }

    #[tokio::test]
    async fn native_http_route_shapes_fail_closed_without_descriptor_dispatch() {
        let (router, _, provider, _) = setup().await;
        for (method, uri) in [
            ("GET", "/future/servers"),
            ("GET", "/compute/servers/extra/path"),
            ("GET", "/compute/servers/%2Fambiguous"),
            ("POST", "/compute/servers/known-id"),
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(method)
                        .uri(uri)
                        .header("authorization", "Bearer project-a")
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert!(
                matches!(
                    response.status(),
                    StatusCode::NOT_FOUND
                        | StatusCode::METHOD_NOT_ALLOWED
                        | StatusCode::BAD_REQUEST
                        | StatusCode::FORBIDDEN
                ),
                "unexpected status for {method} {uri}: {}",
                response.status()
            );
        }
        assert_eq!(provider.instance_count(), 0);
    }

    #[tokio::test]
    async fn native_compute_rejects_foreign_network_before_provider_mutation() {
        let (router, store, provider, foreign_port) = setup().await;
        let body = serde_json::json!({
            "spec": {
                "name": "cross-tenant-network",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": [foreign_port.to_string()]
            }
        });
        let (status, _) = exec(
            &router,
            authed_post("/compute/servers", "a", "foreign-network", body),
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(provider.instance_count(), 0);
        assert!(
            store
                .list_resources("project-a", "compute:server")
                .await
                .expect("resource list")
                .is_empty()
        );
        // The setup-created foreign port is deliberately retained; the
        // rejected request has no path to mutate it or create a relationship.
    }

    #[tokio::test]
    async fn native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation() {
        let (router, _, provider, _) = setup().await;
        let body = serde_json::json!({
            "spec": {
                "name": "security-test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        let missing = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);

        let malformed = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Basic not-a-bearer")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed.status(), StatusCode::UNAUTHORIZED);

        let invalid = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Bearer invalid-token-is-not-used")
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&body).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(provider.instance_count(), 0);
        let malformed_json = router
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/compute/servers")
                    .header("authorization", "Bearer project-a")
                    .header("content-type", "application/json")
                    .body(Body::from(br#"{"spec": malformed}"#.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(malformed_json.status(), StatusCode::BAD_REQUEST);
        assert_eq!(provider.instance_count(), 0);
        let (_, created) = exec(
            &router,
            authed_post("/compute/servers", "a", "isolated", body),
        )
        .await;
        let resource_id = created["resource_id"].as_str().unwrap();
        let operation_id = created["operation_id"].as_str().unwrap();

        let foreign_show = exec(
            &router,
            authed(&format!("/compute/servers/{resource_id}"), "b"),
        )
        .await;
        assert_eq!(foreign_show.0, StatusCode::FORBIDDEN);
        let foreign_operation =
            exec(&router, authed(&format!("/operations/{operation_id}"), "b")).await;
        assert_eq!(foreign_operation.0, StatusCode::NOT_FOUND);

        let unknown = exec(&router, authed("/unknown/servers", "a")).await;
        assert_eq!(unknown.0, StatusCode::NOT_FOUND);

        let cross_scope_replay = exec(
            &router,
            authed_post(
                "/compute/servers",
                "b",
                "isolated",
                serde_json::json!({"spec":{"name":"different"}}),
            ),
        )
        .await;
        assert_eq!(cross_scope_replay.0, StatusCode::BAD_REQUEST);
        assert_eq!(provider.instance_count(), 1);
    }

    #[tokio::test]
    async fn native_compute_delete_returns_operation() {
        let (router, _, provider, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create first
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Set provider timeout so delete becomes async (202)
        provider
            .set_failure(FailureInjection::Timeout)
            .expect("set failure");

        // DELETE → 202 Accepted
        let (status, delete_json) = exec(
            router,
            authed_delete(&format!("/compute/servers/{resource_id}"), "a", "delete-A"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let operation_id = delete_json["operation_id"].as_str().unwrap().to_owned();
        assert!(!delete_json["complete"].as_bool().unwrap());

        // GET /operations/{id} shows the delete
        let (status, op) = exec(router, authed(&format!("/operations/{operation_id}"), "a")).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(op["id"], operation_id);
        assert_eq!(op["action"]["namespace"], "compute");
        assert_eq!(op["action"]["action"], "DeleteServer");
        assert_eq!(op["resource_id"], resource_id);
        assert_eq!(op["owner_scope"]["id"], "project-a");

        // Replay delete with same idempotency key — same operation
        let (status, replay) = exec(
            router,
            authed_delete(&format!("/compute/servers/{resource_id}"), "a", "delete-A"),
        )
        .await;
        assert_eq!(status, StatusCode::ACCEPTED);
        assert_eq!(replay["operation_id"], operation_id);
    }

    #[tokio::test]
    async fn native_compute_create_after_delete_same_key_fails() {
        let (router, _, provider, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Clear failure for synchronous delete
        provider
            .set_failure(FailureInjection::None)
            .expect("clear failure");

        // Delete (synchronous → 204 No Content)
        let response = router
            .clone()
            .oneshot(authed_delete(
                &format!("/compute/servers/{resource_id}"),
                "a",
                "delete-B",
            ))
            .await
            .expect("delete");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Create with SAME key — fail closed: the consumed idempotency key
        // cannot create a new resource, even after the original was deleted.
        let (status, _replay) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body),
        )
        .await;
        // After deletion, the idempotency key is still bound to the original
        // create operation. Replaying returns the original resource (404 not
        // found is expected for a deleted resource — the system rejects the
        // request rather than silently creating a new resource with the same
        // key).
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn native_compute_create_after_delete_new_key_succeeds() {
        let (router, _, provider, _) = setup().await;
        let router = &router;
        let body = serde_json::json!({
            "spec": {
                "name": "test",
                "image_id": "image-a",
                "flavor_id": "00000000-0000-0000-0000-000000000001",
                "network_ids": ["net-a"]
            }
        });

        // Create
        let (status, json) = exec(
            router,
            authed_post("/compute/servers", "a", "create-A", body.clone()),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        let resource_id = json["resource_id"].as_str().unwrap().to_owned();

        // Clear failure for sync delete
        provider
            .set_failure(FailureInjection::None)
            .expect("clear failure");

        // Delete
        let response = router
            .clone()
            .oneshot(authed_delete(
                &format!("/compute/servers/{resource_id}"),
                "a",
                "delete-A",
            ))
            .await
            .expect("delete");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Create with NEW key — starts a new lifecycle
        let (status, recreate) = exec(
            router,
            authed_post("/compute/servers", "a", "create-B", body),
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);
        assert_ne!(recreate["resource_id"].as_str().unwrap(), resource_id);
        assert!(recreate["complete"].as_bool().unwrap());
    }

    #[test]
    fn native_compute_manifest_exposes_no_generation_precondition_mutation() {
        let registry = compute_manifest_registry();
        let manifest = registry.get("compute").expect("compute manifest");
        let actions = manifest
            .actions
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>();
        assert!(
            actions
                .iter()
                .any(|action| action.ends_with(":CreateServer"))
        );
        assert!(
            actions
                .iter()
                .any(|action| action.ends_with(":DeleteServer"))
        );
        assert!(!actions.iter().any(|action| action.contains("Update")));
        assert!(
            !actions
                .iter()
                .any(|action| action.contains("CompareAndSet"))
        );
    }
}
