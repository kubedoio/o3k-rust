//! ISSUE #931 — P15.1 operator-authenticated topology CRUD (in-process half).
//!
//! The real-process half (`p15_1_topology_process.rs`) proves durable
//! convergence/restart/projection/reads/403 with real Keystone password auth
//! against a real `o3kd` subprocess. What it cannot prove offline is
//! operator-authenticated topology mutation: `topology:ManageTopology` is a
//! system-scope operator action, and in a real deployment the bearer that
//! satisfies it is a federated (OIDC) system token — there is no non-OIDC path
//! to a System-scoped token (see `TokenService::issue` /
//! `TokenIssuerAdapter::issue_native`).
//!
//! So this file proves the operator CRUD path through the FULL production
//! stack — `o3k_api::router_with_state`, the real `TopologyGuard`, the real
//! durable `TopologyStore`, the real `StaticAuthorizer::standard()`, and a real
//! mutating audit sink — against a real `O3kStore`, using a `TokenIssuer` that
//! mints the System-scope + `operator`-role `AuthContext`. This is the repo's
//! established mechanism for system-scope identity in in-process tests: it is
//! exactly the `TestIssuer` pattern used by
//! `bins/o3kd/tests/native_diagnostics_process.rs` (see its `TestIssuer`).
//!
//! Runs for real, NOT `#[ignore]`.
#![allow(clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use o3k_kernel::{
    AuditQuery, AuthContext, DurableAuditRepository, LocationRegistry, OwnershipScope, Principal,
    PrincipalId, ScopeId, ScopeKind, StaticAuthorizer, TopologyStore, UserPrincipal,
};
use o3k_native_api::auth::TokenIssuer;
use o3k_native_api::pagination::CursorConfig;
use serde_json::{Value, json};
use tower::ServiceExt;

const CURSOR_KEY: &[u8] = b"p15-1-operator-cursor-signing-key-32-bytes-min";
const OPERATOR: &str = "operator-token";
const TENANT: &str = "tenant-token";

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn operator_context() -> AuthContext {
    AuthContext::new(
        Principal::User(UserPrincipal::new(
            PrincipalId::new_unchecked("user-1"),
            "operator",
            None,
        )),
        OwnershipScope::new(
            ScopeId::new_unchecked("system"),
            ScopeKind::System,
            None,
            None,
        ),
        vec!["operator".to_owned()],
        1,
        2,
        "p15-1-request",
        "p15-1-audit",
        None,
    )
}

fn tenant_context() -> AuthContext {
    AuthContext::new(
        Principal::User(UserPrincipal::new(
            PrincipalId::new_unchecked("user-1"),
            "user-1",
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
        "p15-1-request",
        "p15-1-audit",
        None,
    )
}

/// Mints the credentials used by the routes: a System-scope + `operator`-role
/// principal (the federated system operator) and a plain tenant. This mirrors
/// `native_diagnostics_process.rs::TestIssuer`, the accepted mechanism for
/// representing system-scope identity in in-process tests.
#[derive(Clone)]
struct TestIssuer {
    operator: AuthContext,
    tenant: AuthContext,
}

#[async_trait::async_trait]
impl TokenIssuer for TestIssuer {
    async fn issue_native(
        &self,
        _request: &o3k_native_api::auth::NativeTokenRequestV1,
    ) -> Result<(String, Value), o3k_native_api::error::ProblemDetails> {
        Err(o3k_native_api::error::ProblemDetails::unauthorized())
    }

    async fn auth_context(
        &self,
        token: &str,
    ) -> Result<AuthContext, o3k_native_api::error::ProblemDetails> {
        match token {
            OPERATOR => Ok(self.operator.clone()),
            TENANT => Ok(self.tenant.clone()),
            _ => Err(o3k_native_api::error::ProblemDetails::unauthorized()),
        }
    }
}

/// Builds the production router over a real durable store with a real
/// `TopologyGuard` rebuilt from the store snapshot — the same reconstruction
/// the composition performs at startup. Reusing one store across two builds is
/// how we prove durable reconstruction (p12_6/p12_7 convention).
async fn build_router(
    store: &Arc<o3k_store::unified::O3kStore>,
) -> Result<axum::Router, Box<dyn std::error::Error>> {
    let snapshot = store.load_snapshot().await?;
    let guard =
        o3k_native_api::topology::TopologyGuard::new(LocationRegistry::from_snapshot(snapshot)?);
    let store_dyn: Arc<dyn TopologyStore> = store.clone();
    let native = o3k_native_api::NativeApiState::new(
        None,
        CursorConfig::new(CURSOR_KEY.to_vec())?,
        Some(Arc::new(TestIssuer {
            operator: operator_context(),
            tenant: tenant_context(),
        })),
        None,
        None,
        None,
    )?
    .with_locations(Arc::new(guard))
    .with_topology_store(store_dyn)
    .with_authorizer(Arc::new(StaticAuthorizer::standard()));
    Ok(o3k_api::router_with_state(
        o3k_api::AppState::new().with_native_api(native),
    ))
}

/// Sends one request through the production router; returns `(status, body)`.
async fn request(
    app: &axum::Router,
    method: Method,
    uri: &str,
    token: Option<&str>,
    body: Option<Value>,
    if_match: Option<&str>,
) -> (StatusCode, Value) {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    if let Some(if_match) = if_match {
        builder = builder.header("if-match", if_match);
    }
    let request = match body {
        Some(body) => builder
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                serde_json::to_vec(&body).expect("serialize body"),
            ))
            .expect("body request"),
        None => builder.body(Body::empty()).expect("empty request"),
    };
    let response = app.clone().oneshot(request).await.expect("router request");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 256 * 1024)
        .await
        .expect("body bytes");
    let value: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, value)
}

fn fd_body(id: &str, class: &str, name: &str, az: &str, parent: Option<&str>) -> Value {
    let mut body = json!({"id": id, "class": class, "name": name, "availability_domain": az});
    if let Some(parent) = parent {
        body["parent"] = Value::from(parent);
    }
    body
}

fn assert_fd(doc: &Value, id: &str, class: &str, parent: Option<&str>, name: &str) {
    assert_eq!(doc["id"], Value::from(id), "{doc}");
    assert_eq!(doc["class"], Value::from(class), "{doc}");
    assert_eq!(doc["name"], Value::from(name), "{doc}");
    assert_eq!(doc["availability_domain"], Value::from("az-1"), "{doc}");
    match parent {
        Some(p) => assert_eq!(doc["parent"], Value::from(p), "{doc}"),
        None => assert!(
            doc.get("parent").is_none() || doc["parent"].is_null(),
            "{doc}"
        ),
    }
}

#[tokio::test]
async fn p15_1_topology_operator_crud_over_production_router() -> TestResult {
    let store = Arc::new(o3k_store::unified::O3kStore::connect_sqlite_memory().await?);
    let store_dyn: Arc<dyn TopologyStore> = store.clone();

    // Seed the canonical single region through the kernel guard (persists to
    // the same durable store the routers use), exactly as the composition
    // converges O3K_LOCATIONS at startup.
    {
        let guard = o3k_native_api::topology::TopologyGuard::new(LocationRegistry::default());
        // Seeding direct through the registry, not an HTTP mutation request.
        guard.declare_region(&*store_dyn, "region-a", None).await?;
        guard
            .declare_availability_domain(&*store_dyn, "region-a", "az-1", None)
            .await?;
    }

    let app = build_router(&store).await?;

    // Discovery projection of the seeded single region.
    let (status, regions) = request(&app, Method::GET, "/o3k/v1/regions", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "{regions}");
    assert_eq!(regions["regions"][0]["id"], "region-a");
    assert_eq!(
        regions["regions"][0]["availability_domains"][0]["id"],
        "az-1"
    );

    // ── Operator CRUD: region-b + az-b through the real API ──────────────
    let (status, doc) = request(
        &app,
        Method::PUT,
        "/o3k/v1/regions/region-b",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "declare region-b: {doc}");
    assert_eq!(doc["id"], "region-b");
    let (status, doc) = request(
        &app,
        Method::PUT,
        "/o3k/v1/regions/region-b/availability-domains/az-b",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "declare az-b: {doc}");
    assert_eq!(doc["id"], "region-b");

    // ── Nested failure-domain hierarchy ──────────────────────────────────
    let (status, site) = request(
        &app,
        Method::POST,
        "/o3k/v1/topology/failure-domains",
        Some(OPERATOR),
        Some(fd_body("site-1", "site", "Site One", "az-1", None)),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{site}");
    assert_fd(&site, "site-1", "site", None, "Site One");

    let (status, rack) = request(
        &app,
        Method::POST,
        "/o3k/v1/topology/failure-domains",
        Some(OPERATOR),
        Some(fd_body(
            "rack-1",
            "rack",
            "Rack One",
            "az-1",
            Some("site-1"),
        )),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{rack}");
    assert_fd(&rack, "rack-1", "rack", Some("site-1"), "Rack One");

    let (status, pd) = request(
        &app,
        Method::POST,
        "/o3k/v1/topology/failure-domains",
        Some(OPERATOR),
        Some(fd_body(
            "power-domain-1",
            "power-domain",
            "Power Domain One",
            "az-1",
            Some("rack-1"),
        )),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::CREATED, "{pd}");
    assert_fd(
        &pd,
        "power-domain-1",
        "power-domain",
        Some("rack-1"),
        "Power Domain One",
    );

    // ── Idempotent replay of an identical create converges (200, same doc) ──
    let (status, replay) = request(
        &app,
        Method::POST,
        "/o3k/v1/topology/failure-domains",
        Some(OPERATOR),
        Some(fd_body(
            "rack-1",
            "rack",
            "Rack One",
            "az-1",
            Some("site-1"),
        )),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "idempotent replay: {replay}");
    assert_eq!(replay["id"], "rack-1");

    // ── Binding a resource provider to rack-1 ────────────────────────────
    let (status, binding) = request(
        &app,
        Method::PUT,
        "/o3k/v1/topology/failure-domains/rack-1/bindings/resource-provider/rp-1",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "bind: {binding}");
    assert_eq!(binding["failure_domain"], "rack-1");
    assert_eq!(binding["target"]["kind"], "resource-provider");
    assert_eq!(binding["target"]["id"], "rp-1");

    // ── Update rack-1 with If-Match (optimistic concurrency) ─────────────
    let (status, updated) = request(
        &app,
        Method::PUT,
        "/o3k/v1/topology/failure-domains/rack-1",
        Some(OPERATOR),
        Some(json!({"name": "Rack One Renamed", "metadata": {"row": "42"}})),
        Some("generation-1"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "update: {updated}");
    assert_eq!(updated["name"], "Rack One Renamed");
    assert_eq!(updated["generation"], 2);
    assert_eq!(updated["metadata"]["row"], "42");

    // ── Tenant (project-scoped) is denied every mutation while reads work ──
    for (method, uri, body) in [
        (
            Method::POST,
            "/o3k/v1/topology/failure-domains",
            Some(fd_body("t-fd", "site", "Tenant", "az-1", None)),
        ),
        (Method::PUT, "/o3k/v1/regions/region-tenant", None),
        (
            Method::PUT,
            "/o3k/v1/topology/failure-domains/rack-1",
            Some(json!({"name": "x"})),
        ),
    ] {
        let (status, _) = request(&app, method, uri, Some(TENANT), body, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN, "tenant must be 403 on {uri}");
    }
    let (status, list) = request(
        &app,
        Method::GET,
        "/o3k/v1/topology/failure-domains",
        Some(TENANT),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "tenant read: {list}");

    // ── DELETE semantics: with-children conflicts, leaf succeeds ──────────
    let (status, _) = request(
        &app,
        Method::DELETE,
        "/o3k/v1/topology/failure-domains/site-1",
        Some(OPERATOR),
        None,
        Some("generation-1"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::CONFLICT,
        "deleting a domain with children must conflict"
    );
    let (status, _) = request(
        &app,
        Method::DELETE,
        "/o3k/v1/topology/failure-domains/power-domain-1",
        Some(OPERATOR),
        None,
        Some("generation-1"),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "leaf delete");

    // ── Audit: every mutation is a durable topology:ManageTopology by the
    //    operator, read back from the store's audit surface (audit is folded
    //    into the mutation transaction; there is no separate sink).
    let page = DurableAuditRepository::page(
        &*store,
        &AuditQuery {
            scope: operator_context().effective_scope().clone(),
            after_event_id: None,
            event_id: None,
            limit: 200,
            service: Some("topology".to_owned()),
            action: Some("topology:ManageTopology".to_owned()),
            outcome: Some("succeeded".to_owned()),
            resource_type: None,
            resource_id: None,
            operation_id: None,
            principal_id: None,
            request_id: None,
            audit_id: None,
            from_timestamp: None,
            until_timestamp: None,
        },
    )
    .await?;
    let events = page.events;
    let managed: Vec<_> = events
        .iter()
        .filter(|event| event.principal_id.as_str() == "user-1")
        .collect();
    assert!(
        !managed.is_empty(),
        "no topology:ManageTopology audit events: {events:?}"
    );
    let find = |kind: &str, id: &str| {
        managed.iter().any(|event| {
            event
                .resource_type
                .as_ref()
                .is_some_and(|rt| rt.name() == kind)
                && event
                    .resource_id
                    .as_ref()
                    .is_some_and(|rid| rid.as_str() == id)
        })
    };
    assert!(find("region", "region-b"), "missing region region-b audit");
    assert!(find("availability_domain", "az-b"), "missing az az-b audit");
    assert!(
        find("failure_domain", "site-1"),
        "missing failure_domain site-1 audit"
    );
    assert!(
        find("failure_domain", "rack-1"),
        "missing failure_domain rack-1 audit"
    );
    assert!(find("binding", "rack-1"), "missing binding rack-1 audit");
    // Tenant mutations were denied, so they produced no ManageTopology events:
    assert!(
        !managed.iter().any(|e| e
            .resource_id
            .as_ref()
            .is_some_and(|rid| rid.as_str() == "region-tenant")),
        "denied tenant mutation must not be audited as a topology mutation"
    );

    // ── Reconstruction: new router over the SAME durable store ────────────
    let rebuilt = build_router(&store).await?;
    let (status, regions2) =
        request(&rebuilt, Method::GET, "/o3k/v1/regions", None, None, None).await;
    assert_eq!(status, StatusCode::OK, "{regions2}");
    let region_ids: Vec<&str> = regions2["regions"]
        .as_array()
        .map(|items| items.iter().filter_map(|r| r["id"].as_str()).collect())
        .unwrap_or_default();
    assert!(region_ids.contains(&"region-a"));
    assert!(region_ids.contains(&"region-b"));

    // ids, hierarchy, class/name/metadata, and the binding are unchanged.
    for (id, class, parent, name) in [
        ("site-1", "site", None, "Site One"),
        ("rack-1", "rack", Some("site-1"), "Rack One Renamed"),
    ] {
        let (status, doc) = request(
            &rebuilt,
            Method::GET,
            &format!("/o3k/v1/topology/failure-domains/{id}"),
            Some(OPERATOR),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK, "rebuilt show {id}: {doc}");
        assert_fd(&doc, id, class, parent, name);
    }
    // power-domain-1 was deleted before reconstruction -> gone.
    let (status, _) = request(
        &rebuilt,
        Method::GET,
        "/o3k/v1/topology/failure-domains/power-domain-1",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "deleted leaf must not reappear after reconstruction"
    );

    let (status, bindings2) = request(
        &rebuilt,
        Method::GET,
        "/o3k/v1/topology/failure-domains/rack-1/bindings",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{bindings2}");
    assert_eq!(bindings2["items"][0]["target"]["id"], "rp-1");
    assert_eq!(bindings2["items"][0]["target"]["kind"], "resource-provider");

    assert_eq!(bindings2["items"][0]["failure_domain"], "rack-1");
    // Operator can still mutate after reconstruction.
    let (status, _) = request(
        &rebuilt,
        Method::PUT,
        "/o3k/v1/regions/region-c",
        Some(OPERATOR),
        None,
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "region-c after reconstruction");

    println!("P15.1 topology operator CRUD over production router: PASS");
    Ok(())
}
