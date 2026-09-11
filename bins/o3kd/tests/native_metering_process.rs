//! Real-adapter, real-router HTTP journey for the native metering endpoints
//! (#904). Proves the production `o3kd` metering adapter
//! (`o3kd::native_adapters::MeteringAdapter`) serves `/o3k/v1/metering/*` over
//! the real production `o3k_api::router_with_state` with the real
//! `StaticAuthorizer`, and that lifecycle observations recorded through the
//! same adapter produce exact, non-double-counted usage.
//!
//! A controllable fake clock is installed in the adapter so the usage
//! arithmetic is provable without sleeping; the clock is the same object the
//! handler's authority reads through, so quantities are exact.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use o3k_kernel::{
    AuthContext, Clock, LifecycleMeteringObserver, ManifestRegistry, OwnershipScope, Principal,
    PrincipalId, ScopeId, ScopeKind, UserPrincipal,
};
use o3k_native_api::auth::TokenIssuer;
use o3k_native_api::pagination::CursorConfig;
use serde_json::Value;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tower::ServiceExt;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const DEFINITIONS: &str = "/o3k/v1/metering/definitions";
const HOUR_MS: i64 = 3_600_000;
/// A UTC hour boundary: 2023-11-14T22:00:00Z.
const BASE_MS: i64 = 1_699_999_200_000;

/// Never present in any public metering DTO; asserted against every 200 body.
const SECRET_MARKERS: [&str; 6] = [
    "password",
    "private_key",
    "postgres://",
    "Bearer ",
    "node-42",
    "agent-epoch-9",
];

/// Fake clock the test drives directly. The adapter reads it for every
/// observation, so advancing it deterministically moves the durable intervals.
#[derive(Clone)]
struct TestClock(Arc<AtomicI64>);

impl TestClock {
    fn new(now_ms: i64) -> Self {
        Self(Arc::new(AtomicI64::new(now_ms)))
    }

    fn set(&self, now_ms: i64) {
        self.0.store(now_ms, Ordering::SeqCst);
    }
}

impl Clock for TestClock {
    fn now_unix_ms(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
}

/// Mock `TokenIssuer` keyed by token string: a durable system operator and a
/// plain project tenant.
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
            "operator-token" => Ok(self.operator.clone()),
            "tenant-token" => Ok(self.tenant.clone()),
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

struct Runtime {
    app: axum::Router,
    adapter: Arc<o3kd::native_adapters::MeteringAdapter>,
    clock: Arc<TestClock>,
}

async fn build_runtime(now_ms: i64) -> Result<Runtime, Box<dyn std::error::Error>> {
    let store = Arc::new(o3k_store::unified::O3kStore::connect_sqlite_memory().await?);
    // Anchor the authority window at the fake clock's start so a query at the
    // first bucket is `complete` rather than `partial`.
    o3k_kernel::MeteringRepository::ensure_authority(&*store, now_ms).await?;

    let clock = Arc::new(TestClock::new(now_ms));
    let adapter = Arc::new(o3kd::native_adapters::MeteringAdapter::new(
        store,
        clock.clone(),
    ));

    let mut manifests = ManifestRegistry::new();
    manifests.seed_core()?;

    let token_issuer: Arc<dyn TokenIssuer> = Arc::new(TestIssuer {
        operator: auth_context("system", ScopeKind::System, &["operator"]),
        tenant: auth_context("project-a", ScopeKind::Project, &["member"]),
    });

    let native = o3k_native_api::NativeApiState::new(
        Some(manifests),
        CursorConfig::new(b"test-only-native-cursor-key-at-least-32-bytes".to_vec())?,
        Some(token_issuer),
        None,
        None,
        None,
    )?
    .with_metering_reader(adapter.clone())
    .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));

    Ok(Runtime {
        app: o3k_api::router_with_state(o3k_api::AppState::new().with_native_api(native)),
        adapter,
        clock,
    })
}

async fn get(
    app: &axum::Router,
    uri: &str,
    token: Option<&str>,
) -> Result<(StatusCode, String), Box<dyn std::error::Error>> {
    let mut builder = Request::builder().uri(uri);
    if let Some(token) = token {
        builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = app.clone().oneshot(builder.body(Body::empty())?).await?;
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024).await?;
    Ok((status, String::from_utf8_lossy(&bytes).into_owned()))
}

fn instant(unix_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(unix_ms)
        .expect("representable instant")
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn usage_uri(start_ms: i64, end_ms: i64, scope: Option<&str>) -> String {
    let mut uri = format!(
        "/o3k/v1/metering/usage?meter=compute:instance_seconds&start={}&end={}",
        instant(start_ms),
        instant(end_ms),
    );
    if let Some(scope) = scope {
        uri.push_str("&scope=");
        uri.push_str(scope);
    }
    uri
}

/// The catalog is public to any authenticated caller, usage is owner-scoped,
/// cross-scope selection is durable system/operator authority, and every 200
/// body is secret-free.
#[tokio::test]
async fn metering_http_definitions_and_scope_authority() -> TestResult {
    let runtime = build_runtime(BASE_MS).await?;
    let app = runtime.app;

    // No credential: unauthenticated for both endpoints.
    let unauthenticated = [
        DEFINITIONS.to_owned(),
        usage_uri(BASE_MS, BASE_MS + HOUR_MS, None),
    ];
    for uri in &unauthenticated {
        let (status, _) = get(&app, uri, None).await?;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{uri}");
    }

    // A plain tenant reads the public catalog and sees both canonical meters.
    let (status, body) = get(&app, DEFINITIONS, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    let definitions = value["definitions"]
        .as_array()
        .ok_or("definitions must be an array")?;
    assert_eq!(definitions.len(), 2, "{body}");
    assert_eq!(definitions[0]["key"], "compute:instance_seconds");
    assert_eq!(definitions[0]["unit"], "instance_second");
    assert_eq!(definitions[1]["key"], "volume:allocated_byte_seconds");
    assert_eq!(definitions[1]["unit"], "byte_second");

    // The tenant reads its own scope with no `scope` parameter.
    let (status, body) = get(
        &app,
        &usage_uri(BASE_MS, BASE_MS + HOUR_MS, None),
        Some("tenant-token"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-a");
    for secret in SECRET_MARKERS {
        assert!(
            !body.contains(secret),
            "{secret:?} leaked into a usage body"
        );
    }

    // The same tenant selecting another project is denied outright.
    let (status, _) = get(
        &app,
        &usage_uri(BASE_MS, BASE_MS + HOUR_MS, Some("project-b")),
        Some("tenant-token"),
    )
    .await?;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // A durable system+operator may read another scope.
    let (status, body) = get(
        &app,
        &usage_uri(BASE_MS, BASE_MS + HOUR_MS, Some("project-b")),
        Some("operator-token"),
    )
    .await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["scope"], "project-b");
    for secret in SECRET_MARKERS {
        assert!(
            !body.contains(secret),
            "{secret:?} leaked into a usage body"
        );
    }
    Ok(())
}

/// A full journey through the real adapter: ACTIVE opens the interval, the
/// in-window query reports the exact expected quantity and unit as RFC3339 UTC
/// instants, and SHUTOFF closes the interval so the same query returns exactly
/// the same total with no double count.
#[tokio::test]
async fn metering_http_usage_journey_is_exact_and_replay_safe() -> TestResult {
    let Runtime {
        app,
        adapter,
        clock,
    } = build_runtime(BASE_MS).await?;
    let scope = "project-a";
    let uri = usage_uri(BASE_MS, BASE_MS + HOUR_MS, None);

    clock.set(BASE_MS);
    adapter
        .observe_resource_state("compute_instance", scope, "server-1", "ACTIVE")
        .await?;

    // The adapter evaluates queries at its own clock, so advance to the window
    // end to observe the whole open in-window segment.
    clock.set(BASE_MS + HOUR_MS);
    // While running, the open in-window segment is reported.
    let (status, body) = get(&app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    let running = &value[0];
    assert_eq!(running["status"], "complete", "{body}");
    assert_eq!(running["unit"], "instance_second");
    assert_eq!(running["aggregation"], "integral");
    assert_eq!(running["total"], "3600.000");
    assert_eq!(running["buckets"][0]["quantity"], "3600.000");
    for field in ["start", "end", "observed_through", "bucket_start"] {
        let text = if field == "bucket_start" {
            running["buckets"][0]["bucket_start"]
                .as_str()
                .ok_or("bucket_start")?
        } else {
            running[field].as_str().ok_or("instant")?
        };
        assert!(text.ends_with('Z'), "{field} is not UTC: {text}");
        assert!(
            chrono::DateTime::parse_from_rfc3339(text).is_ok(),
            "{field} is not RFC3339: {text}"
        );
    }

    // SHUTOFF closes the interval; the identical query must not double count.
    clock.set(BASE_MS + HOUR_MS);
    adapter
        .observe_resource_state("compute_instance", scope, "server-1", "SHUTOFF")
        .await?;
    let (status, body) = get(&app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(
        value[0]["total"], "3600.000",
        "SHUTOFF must close the interval, not accrue a second hour: {body}"
    );
    assert_eq!(value[0]["status"], "complete");

    // Replaying the close is an idempotent no-op through the real authority.
    adapter
        .observe_resource_state("compute_instance", scope, "server-1", "SHUTOFF")
        .await?;
    let (status, body) = get(&app, &uri, Some("tenant-token")).await?;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value[0]["total"], "3600.000", "{body}");

    for secret in SECRET_MARKERS {
        assert!(
            !body.contains(secret),
            "{secret:?} leaked into a usage body"
        );
    }
    Ok(())
}

/// The adapter maps a state it cannot decode to a hard corrupt error rather
/// than silently idling the meter, and an unknown resource kind is a no-op.
#[tokio::test]
async fn metering_http_adapter_refuses_undecodable_states() -> TestResult {
    let Runtime { adapter, .. } = build_runtime(BASE_MS).await?;
    let error = adapter
        .observe_resource_state("compute_instance", "project-a", "server-1", "WOBBLING")
        .await
        .expect_err("an unknown state must fail closed");
    assert!(matches!(error, o3k_kernel::KernelError::MeteringCorrupt(_)));

    adapter
        .observe_resource_state("network_network", "project-a", "net-1", "ACTIVE")
        .await
        .expect("a non-metered kind is a no-op");
    Ok(())
}
