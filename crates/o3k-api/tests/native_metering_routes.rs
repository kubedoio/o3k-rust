//! End-to-end routing and authorization for the native metering endpoints
//! (#904), mounted by the production `o3k_api::router_with_state` under
//! `/o3k/v1/metering`.
//!
//! Covers the authorization boundaries that make metering safe: any
//! authenticated user may read the public meter catalog; a caller reads usage
//! for *its own* effective scope without any scope parameter; and cross-scope
//! selection is explicit durable system/operator authority, denied for a
//! project-scoped caller even when it holds an `operator` role name. Also
//! asserts that no 200 body leaks credential material or provider identity.

use axum::body::{Body, to_bytes};
use http::{Request, StatusCode};
use o3k_kernel::{
    ActionId, AuthContext, Authorizer, MeterUsage, MeterUsageReport, OwnershipScope, Principal,
    PrincipalId, PrincipalKind, ScopeId, ScopeKind, UsageBucket, UsageQuery, UsageStatus,
    UserPrincipal,
};
use serde_json::Value;
use std::sync::{Arc, Mutex};
use tower::ServiceExt;

use o3k_native_api::metering::{
    METERING_VERSION, MeterDefinitionView, MeterDefinitionsPage, MeteringError, MeteringReader,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DEFINITIONS: &str = "/o3k/v1/metering/definitions";
const USAGE: &str = "/o3k/v1/metering/usage?meter=compute:instance_seconds\
&start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";

/// Never present in any public metering DTO; asserted against every 200 body.
const SECRET_MARKERS: [&str; 6] = [
    "password",
    "private_key",
    "postgres://",
    "Bearer ",
    "node-42",
    "agent-epoch-9",
];

#[derive(Clone)]
struct TestIssuer {
    tenant: AuthContext,
    tenant_operator: AuthContext,
    system: AuthContext,
    operator: AuthContext,
}

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
        token: &str,
    ) -> Result<AuthContext, o3k_native_api::error::ProblemDetails> {
        match token {
            "tenant-token" => Ok(self.tenant.clone()),
            "tenant-operator-token" => Ok(self.tenant_operator.clone()),
            "system-token" => Ok(self.system.clone()),
            "operator-token" => Ok(self.operator.clone()),
            _ => Err(o3k_native_api::error::ProblemDetails::unauthorized()),
        }
    }
}

fn auth_context(scope_id: &str, scope_kind: ScopeKind, roles: &[&str]) -> AuthContext {
    AuthContext::new(
        Principal::User(UserPrincipal::new(
            PrincipalId::new_unchecked("user-1"),
            "user-1",
            None,
        )),
        OwnershipScope::new(ScopeId::new_unchecked(scope_id), scope_kind, None, None),
        roles.iter().map(|role| (*role).to_owned()).collect(),
        1,
        2,
        "audit-test",
        "request-test",
        None,
    )
}

/// Meters the active profile can produce. Deliberately a strict subset of
/// `METER_CATALOG`, so the omitted-`meter` path is proven to come from the
/// reader rather than from the static catalog.
const PRODUCIBLE: [&str; 1] = ["compute:instance_seconds"];

/// Resource series owned by a scope. A resource from another scope must be
/// indistinguishable from an empty series.
const RESOURCES: [(&str, &str); 2] = [("server-own", "project-a"), ("server-foreign", "project-b")];

/// Fake metering authority that records the exact query it received so tests
/// can prove the handler derived scope from `AuthContext`, not from the wire.
struct FakeMeteringReader {
    last_usage_query: Mutex<Option<UsageQuery>>,
}

impl FakeMeteringReader {
    fn new() -> Self {
        Self {
            last_usage_query: Mutex::new(None),
        }
    }

    fn last_query(&self) -> Option<UsageQuery> {
        self.last_usage_query
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
    }
}

#[async_trait::async_trait]
impl MeteringReader for FakeMeteringReader {
    async fn definitions(
        &self,
        limit: usize,
        after: Option<&str>,
    ) -> Result<MeterDefinitionsPage, MeteringError> {
        let mut definitions: Vec<_> = PRODUCIBLE
            .iter()
            .filter_map(|key| o3k_kernel::meter_definition(key))
            .map(MeterDefinitionView::from_definition)
            .filter(|definition| after.is_none_or(|after| definition.key.as_str() > after))
            .collect();
        let has_more = definitions.len() > limit;
        definitions.truncate(limit);
        Ok(MeterDefinitionsPage {
            definitions,
            has_more,
            next_cursor: None,
        })
    }

    async fn usage(&self, query: &UsageQuery) -> Result<MeterUsageReport, MeteringError> {
        if let Ok(mut guard) = self.last_usage_query.lock() {
            *guard = Some(query.clone());
        }
        // A resource id that does not belong to this scope is a zero series,
        // never a disclosure of the foreign resource.
        let narrowed_away = query.resource_id.as_deref().is_some_and(|resource_id| {
            !RESOURCES
                .iter()
                .any(|(id, scope)| *id == resource_id && *scope == query.scope)
        });
        let meters = query
            .meter_keys
            .iter()
            .map(|key| {
                let definition = o3k_kernel::meter_definition(key).ok_or(MeteringError::Corrupt)?;
                let (total, buckets) = if narrowed_away {
                    ("0.000".to_owned(), Vec::new())
                } else {
                    (
                        "2.500".to_owned(),
                        vec![UsageBucket {
                            bucket_start_ms: query.start_ms,
                            bucket_width_ms: query.granularity.width_ms(),
                            quantity: "2.500".to_owned(),
                        }],
                    )
                };
                Ok(MeterUsage {
                    meter_key: key.clone(),
                    unit: definition.unit,
                    granularity: query.granularity,
                    status: UsageStatus::Complete,
                    buckets,
                    total,
                })
            })
            .collect::<Result<Vec<_>, MeteringError>>()?;
        Ok(MeterUsageReport {
            scope: query.scope.clone(),
            start_ms: query.start_ms,
            end_ms: query.end_ms,
            observed_through_ms: query.end_ms,
            authority_started_at_ms: Some(query.start_ms),
            last_observed_at_ms: None,
            meters,
        })
    }
}

struct Fixture {
    app: axum::Router,
    reader: Arc<FakeMeteringReader>,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error>> {
    let reader = Arc::new(FakeMeteringReader::new());
    let native = o3k_native_api::NativeApiState::new(
        None,
        o3k_native_api::pagination::CursorConfig::default(),
        Some(Arc::new(TestIssuer {
            tenant: auth_context("project-a", ScopeKind::Project, &["member"]),
            tenant_operator: auth_context("project-b", ScopeKind::Project, &["operator"]),
            system: auth_context("system", ScopeKind::System, &["admin"]),
            operator: auth_context("system", ScopeKind::System, &["operator"]),
        })),
        None,
        None,
        None,
    )?
    .with_metering_reader(reader.clone())
    .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
    Ok(Fixture {
        app: o3k_api::router_with_state(o3k_api::AppState::new().with_native_api(native)),
        reader,
    })
}

async fn get(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header("authorization", format!("Bearer {token}"));
    }
    let response = app.clone().oneshot(builder.body(Body::empty())?).await?;
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 16_384).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

#[tokio::test]
async fn native_metering_routes_require_authentication() -> TestResult {
    let fixture = fixture()?;
    for uri in [DEFINITIONS, USAGE] {
        let (status, _) = get(&fixture.app, uri, None).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }
    Ok(())
}

#[tokio::test]
async fn any_authenticated_tenant_can_read_the_public_definitions() -> TestResult {
    let fixture = fixture()?;
    let (status, body) = get(&fixture.app, DEFINITIONS, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert!(
        value["definitions"]
            .as_array()
            .is_some_and(|defs| !defs.is_empty()),
        "definitions must be a non-empty bounded page"
    );
    assert!(value.get("has_more").is_some());
    for secret in SECRET_MARKERS {
        assert!(!body.contains(secret), "{secret:?} leaked into definitions");
    }
    Ok(())
}

#[tokio::test]
async fn tenant_reads_its_own_scope_without_a_scope_parameter() -> TestResult {
    let fixture = fixture()?;
    let (status, body) = get(&fixture.app, USAGE, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    // One response object per requested meter, rendered as a JSON array.
    let Some(responses) = value.as_array() else {
        return Err("usage response is an array".into());
    };
    assert_eq!(responses.len(), 1);
    assert_eq!(responses[0]["scope"], "project-a");
    assert_eq!(responses[0]["status"], "complete");
    assert_eq!(responses[0]["meter_key"], "compute:instance_seconds");
    assert_eq!(responses[0]["granularity"], "hour");
    assert_eq!(responses[0]["unit"], "instance_second");
    assert_eq!(responses[0]["aggregation"], "integral");
    // Instants are RFC3339 UTC strings, never raw epoch numbers.
    assert!(
        responses[0]["start"]
            .as_str()
            .is_some_and(|start| start.ends_with('Z'))
    );

    // The reader received the caller's effective scope derived from
    // AuthContext, never a value from the request.
    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(query.scope, "project-a");

    for secret in SECRET_MARKERS {
        assert!(
            !body.contains(secret),
            "{secret:?} leaked into a usage body"
        );
    }
    Ok(())
}

#[tokio::test]
async fn project_scoped_caller_cannot_select_another_scope() -> TestResult {
    let fixture = fixture()?;
    // Genuinely foreign to both project-scoped callers used below
    // (tenant-token is project-a, tenant-operator-token is project-b).
    let uri = format!("{}&scope=project-foreign", USAGE);

    // A plain project tenant supplying a foreign scope is denied outright,
    // never silently downgraded to its own scope.
    let (status, body) = get(&fixture.app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body_status_code(&body)?, 403);

    // Holding an `operator` role NAME inside a project does not change that:
    // cross-scope selection is scope-based durable authority, not role-string.
    let (status, _) = get(&fixture.app, &uri, Some("tenant-operator-token")).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Definitions remain readable for the same caller: the denial is scoped
    // to usage selection, not a general lockout.
    let (status, _) = get(&fixture.app, DEFINITIONS, Some("tenant-operator-token")).await?;
    assert_eq!(status, StatusCode::OK);
    Ok(())
}

#[tokio::test]
async fn explicit_scope_equal_to_the_callers_own_scope_is_allowed() -> TestResult {
    let fixture = fixture()?;
    // tenant-token's effective scope is project-a; naming it explicitly is a
    // no-op convenience, not a cross-scope read.
    let uri = format!("{}&scope=project-a", USAGE);
    let (status, body) = get(&fixture.app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-a");

    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(query.scope, "project-a");
    Ok(())
}

#[tokio::test]
async fn system_scope_without_operator_role_cannot_select_scope() -> TestResult {
    let fixture = fixture()?;
    let uri = format!("{}&scope=project-a", USAGE);
    let (status, _) = get(&fixture.app, &uri, Some("system-token")).await?;
    assert_eq!(status, StatusCode::FORBIDDEN);
    Ok(())
}

#[tokio::test]
async fn system_operator_can_select_another_scope() -> TestResult {
    let fixture = fixture()?;
    let uri = format!("{}&scope=project-a", USAGE);
    let (status, body) = get(&fixture.app, &uri, Some("operator-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-a");

    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(query.scope, "project-a");

    for secret in SECRET_MARKERS {
        assert!(
            !body.contains(secret),
            "{secret:?} leaked into a usage body"
        );
    }
    Ok(())
}

#[tokio::test]
async fn usage_rejects_bad_alignment_and_unknown_meters() -> TestResult {
    let fixture = fixture()?;
    // Misaligned end instant: the client must be explicit, not silently fixed.
    let misaligned = "/o3k/v1/metering/usage?meter=compute:instance_seconds\
&start=2023-11-14T22:00:00Z&end=2023-11-14T22:30:00Z";
    let (status, _) = get(&fixture.app, misaligned, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let unknown = "/o3k/v1/metering/usage?meter=compute:cpu_cycles\
&start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";
    let (status, _) = get(&fixture.app, unknown, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let missing_range = "/o3k/v1/metering/usage?meter=compute:instance_seconds";
    let (status, _) = get(&fixture.app, missing_range, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn multiple_meter_values_return_one_response_per_meter_in_request_order() -> TestResult {
    let fixture = fixture()?;
    let uri = "/o3k/v1/metering/usage\
?meter=volume:allocated_byte_seconds&meter=compute:instance_seconds\
&start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";
    let (status, body) = get(&fixture.app, uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    let Some(responses) = value.as_array() else {
        return Err("usage response is an array".into());
    };
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["meter_key"], "volume:allocated_byte_seconds");
    assert_eq!(responses[1]["meter_key"], "compute:instance_seconds");

    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(
        query.meter_keys,
        vec![
            "volume:allocated_byte_seconds".to_owned(),
            "compute:instance_seconds".to_owned(),
        ],
        "request order must be preserved"
    );
    Ok(())
}

#[tokio::test]
async fn omitted_meter_defaults_to_the_producible_set_in_order() -> TestResult {
    let fixture = fixture()?;
    let uri = "/o3k/v1/metering/usage?start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";
    let (status, body) = get(&fixture.app, uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    let Some(responses) = value.as_array() else {
        return Err("usage response is an array".into());
    };
    // The producible set is read from the authority, not the static catalog:
    // PRODUCIBLE is a strict subset of METER_CATALOG, so using the catalog
    // would produce more responses than this.
    assert_eq!(responses.len(), PRODUCIBLE.len());
    for (response, key) in responses.iter().zip(PRODUCIBLE) {
        assert_eq!(response["meter_key"], key);
    }
    assert!(
        PRODUCIBLE.len() < o3k_kernel::METER_CATALOG.len(),
        "fixture must exercise a strict subset of the catalog"
    );

    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(
        query.meter_keys,
        PRODUCIBLE
            .iter()
            .map(|key| (*key).to_owned())
            .collect::<Vec<_>>()
    );
    Ok(())
}

#[tokio::test]
async fn resource_id_narrowing_is_scoped_and_does_not_reveal_foreign_resources() -> TestResult {
    let fixture = fixture()?;
    let base = "/o3k/v1/metering/usage?meter=compute:instance_seconds\
&start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";

    // A resource owned by another scope yields a well-formed zero result for
    // the caller's own scope and never mentions the foreign resource.
    let foreign_uri = format!("{base}&resource_id=server-foreign");
    let (status, body) = get(&fixture.app, &foreign_uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-a");
    assert_eq!(value[0]["total"], "0.000");
    assert!(
        !body.contains("server-foreign"),
        "foreign resource identity leaked into the response"
    );
    let Some(query) = fixture.reader.last_query() else {
        return Err("reader saw a query".into());
    };
    assert_eq!(query.resource_id.as_deref(), Some("server-foreign"));

    // The caller's own resource filters to real usage.
    let own_uri = format!("{base}&resource_id=server-own");
    let (status, body) = get(&fixture.app, &own_uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-a");
    assert_eq!(value[0]["total"], "2.500");
    Ok(())
}

#[tokio::test]
async fn unknown_query_parameter_is_rejected() -> TestResult {
    let fixture = fixture()?;
    // A typo must not silently degrade to the caller's own scope.
    let uri = "/o3k/v1/metering/usage?meter=compute:instance_seconds\
&scopes=project-b&start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z";
    let (status, _) = get(&fixture.app, uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Rejected at extraction: the authority never sees a query.
    assert!(fixture.reader.last_query().is_none());
    Ok(())
}

#[tokio::test]
async fn cross_scope_read_capability_is_discoverable_in_the_authorization_inventory() {
    let capabilities = o3k_kernel::StaticAuthorizer::standard().capabilities();

    let read_usage = capabilities
        .iter()
        .find(|policy| policy.action == ActionId::new_unchecked("metering", "ReadUsage"));
    assert!(
        read_usage.is_some(),
        "metering:ReadUsage must be discoverable"
    );
    if let Some(policy) = read_usage {
        assert!(policy.require_ownership);
        assert!(policy.required_roles.is_empty());
    }

    let read_usage_all = capabilities
        .iter()
        .find(|policy| policy.action == ActionId::new_unchecked("metering", "ReadUsageAll"));
    assert!(
        read_usage_all.is_some(),
        "cross-scope capability must be discoverable, not ad-hoc handler logic"
    );
    if let Some(policy) = read_usage_all {
        assert!(!policy.require_ownership);
        assert_eq!(policy.required_roles, vec!["operator".to_owned()]);
        assert_eq!(policy.accepted_principals, vec![PrincipalKind::User]);
    }
}

#[tokio::test]
async fn too_many_meters_are_rejected() -> TestResult {
    let fixture = fixture()?;
    let mut uri = String::from("/o3k/v1/metering/usage?");
    for _ in 0..9 {
        uri.push_str("meter=compute:instance_seconds&");
    }
    uri.push_str("start=2023-11-14T22:00:00Z&end=2023-11-14T23:00:00Z");
    let (status, _) = get(&fixture.app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    Ok(())
}

#[tokio::test]
async fn definitions_limit_is_bounded() -> TestResult {
    let fixture = fixture()?;
    for (uri, expected) in [
        (
            "/o3k/v1/metering/definitions?limit=0",
            StatusCode::BAD_REQUEST,
        ),
        (
            "/o3k/v1/metering/definitions?limit=201",
            StatusCode::BAD_REQUEST,
        ),
        ("/o3k/v1/metering/definitions?limit=1", StatusCode::OK),
    ] {
        let (status, _) = get(&fixture.app, uri, Some("tenant-token")).await?;
        assert_eq!(status, expected, "{uri}");
    }
    Ok(())
}

#[tokio::test]
async fn definitions_version_constant_is_exposed() {
    assert_eq!(METERING_VERSION, "v1");
}

fn body_status_code(body: &str) -> Result<u16, Box<dyn std::error::Error>> {
    let value: Value = serde_json::from_str(body)?;
    Ok(value["status"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
        .unwrap_or(0))
}
