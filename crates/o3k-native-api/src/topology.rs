//! Native topology authority API (P15.1, ADR-0184 / SPEC-0047).
//!
//! Exposes the canonical O3K location topology — regions, availability
//! domains, failure domains, and topology bindings — over the native API.
//! O3K is the single authority for this topology; nothing here is derived
//! from hosts, providers, hypervisors, or backends.
//!
//! All durable mutations flow through [`TopologyGuard`], which serializes
//! topology mutations process-wide (the mutex is held across the durable
//! store call, as the kernel requires) and delegates validation plus
//! memory application to [`LocationRegistry`]. Reads of collections are
//! bounded keyset pages over the durable [`TopologyStore`] port.
//!
//! Authorization: `topology:ReadTopology` is open to any authenticated
//! principal; `topology:ManageTopology` is durable system/operator
//! administration (System scope plus the `operator` role), enforced by the
//! Cloud Kernel authorizer, never by this handler. Reads are
//! authenticated-principal-wide: because any principal may read, binding
//! references (provider/host/fabric/storage ids) are visible to any
//! authenticated principal. A tenant-safe projection of topology can be
//! introduced later if needed; the authorization is not changed now.
//!
//! Error responses are RFC 9457 Problem Details; store internals, connection
//! information, and SQL never leave the process.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use o3k_kernel::{
    ActionId, AuditEvent, AuditOutcome, AuthContext, AuthorizationDecision, AuthorizationRequest,
    BindingListCursor, BindingTargetKind, FailureDomain, FailureDomainClass, KernelError,
    LocationError, LocationRegistry, RegionDeclaration, ResourceId, ResourceTarget, ResourceType,
    ServiceNamespace, TopologyBinding, TopologyError, TopologySnapshot, TopologyStore,
};
use serde::Deserialize;

use crate::{
    NativeApiState,
    auth::BearerAuth,
    error::{ErrorCode, ProblemDetails},
    pagination::RepositoryPage,
};

/// Canonical topology action identities (contracts/cloud-kernel-actions.yaml).
pub const ACTION_READ: &str = "ReadTopology";
pub const ACTION_MANAGE: &str = "ManageTopology";

/// Resource-type identity used for authorization and cursor binding.
fn topology_resource_type() -> ResourceType {
    ResourceType::new_unchecked("topology", "failure_domain")
}

fn read_action() -> ActionId {
    ActionId::new_unchecked("topology", ACTION_READ)
}

fn manage_action() -> ActionId {
    ActionId::new_unchecked("topology", ACTION_MANAGE)
}

fn collection_target() -> ResourceTarget {
    ResourceTarget::collection(topology_resource_type(), None)
}

fn instance_target(id: &str) -> ResourceTarget {
    ResourceTarget::instance(
        topology_resource_type(),
        ResourceId::new_unchecked(id),
        None,
    )
}

/// Serialize mutations on the canonical topology registry.
///
/// The kernel splits its API: region mutations take `&mut self`, failure
/// domains and bindings take `&self` (interior `RwLock`). The guard unifies
/// both behind one async mutex so every durable mutation — region, AZ,
/// failure domain, or binding — is globally serialized *including* its
/// durable store call, which the kernel port requires for a single durable
/// mutation sequence.
pub struct TopologyGuard {
    inner: tokio::sync::Mutex<LocationRegistry>,
}

impl TopologyGuard {
    #[must_use]
    pub fn new(registry: LocationRegistry) -> Self {
        Self {
            inner: tokio::sync::Mutex::new(registry),
        }
    }

    /// Returns the complete validated topology snapshot.
    pub async fn snapshot(&self) -> TopologySnapshot {
        self.inner.lock().await.snapshot()
    }

    /// Returns the canonical region declarations (sorted by id).
    pub async fn read_regions(&self) -> Vec<RegionDeclaration> {
        self.inner.lock().await.snapshot().regions
    }

    /// Returns a clone of the region declaration with the given id.
    pub async fn region(&self, id: &str) -> Option<RegionDeclaration> {
        self.inner.lock().await.region(id).cloned()
    }

    /// Returns a clone of the failure domain with the given id.
    pub async fn failure_domain(&self, id: &str) -> Option<FailureDomain> {
        self.inner.lock().await.failure_domain(id)
    }

    /// Returns true when the failure-domain identifier is configured.
    pub async fn contains_failure_domain(&self, id: &str) -> bool {
        self.inner.lock().await.contains_failure_domain(id)
    }

    /// Returns true when `region` declares availability domain `az`.
    pub async fn region_declares_az(&self, region: &str, az: &str) -> bool {
        self.inner
            .lock()
            .await
            .availability_domains_of(region)
            .iter()
            .any(|known| known.id == az)
    }

    pub async fn declare_region(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .declare_region(store, id, audit)
            .await
    }

    pub async fn remove_region(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .remove_region(store, id, audit)
            .await
    }

    pub async fn declare_availability_domain(
        &self,
        store: &dyn TopologyStore,
        region_id: &str,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .declare_availability_domain(store, region_id, az_id, audit)
            .await
    }

    pub async fn remove_availability_domain(
        &self,
        store: &dyn TopologyStore,
        az_id: &str,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .remove_availability_domain(store, az_id, audit)
            .await
    }

    pub async fn create_failure_domain(
        &self,
        store: &dyn TopologyStore,
        domain: FailureDomain,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .create_failure_domain(store, domain, audit)
            .await
    }

    pub async fn update_failure_domain(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        name: String,
        metadata: BTreeMap<String, String>,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .update_failure_domain(store, id, &name, metadata, expected_generation, audit)
            .await
    }

    pub async fn delete_failure_domain(
        &self,
        store: &dyn TopologyStore,
        id: &str,
        expected_generation: u64,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner
            .lock()
            .await
            .delete_failure_domain(store, id, expected_generation, audit)
            .await
    }

    pub async fn bind(
        &self,
        store: &dyn TopologyStore,
        binding: TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner.lock().await.bind(store, binding, audit).await
    }

    pub async fn unbind(
        &self,
        store: &dyn TopologyStore,
        binding: TopologyBinding,
        audit: Option<&AuditEvent>,
    ) -> Result<(), TopologyError> {
        self.inner.lock().await.unbind(store, binding, audit).await
    }
}

// ── Request DTOs ───────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageQuery {
    pub limit: Option<String>,
    pub cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDomainCreateRequest {
    pub id: String,
    pub class: String,
    pub name: String,
    pub availability_domain: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FailureDomainUpdateRequest {
    pub name: String,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Authorizes against the configured Cloud Kernel authorizer. Fails closed
/// when no authorizer is configured (mirrors the quota handler).
fn authorize(
    state: &NativeApiState,
    auth: &AuthContext,
    action: ActionId,
    target: ResourceTarget,
) -> bool {
    let Some(authorizer) = state.authorizer.as_ref() else {
        return false;
    };
    matches!(
        authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: target,
        }),
        AuthorizationDecision::Allow
    )
}

fn parse_class(raw: &str) -> Option<FailureDomainClass> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok()
}

fn parse_kind(raw: &str) -> Option<BindingTargetKind> {
    serde_json::from_value(serde_json::Value::String(raw.to_owned())).ok()
}

/// Parses the optimistic-concurrency precondition `If-Match: generation-N`,
/// mirroring the generic compute update convention. Returns `None` when the
/// header is missing or malformed.
fn parse_if_match_generation(headers: &HeaderMap) -> Option<u64> {
    let value = headers.get("if-match")?.to_str().ok()?;
    let generation: i64 = value.strip_prefix("generation-")?.parse().ok()?;
    u64::try_from(generation)
        .ok()
        .filter(|generation| *generation > 0)
}

/// Maps kernel location-validation failures to stable public error codes.
fn location_error_code(error: &LocationError) -> ErrorCode {
    match error {
        LocationError::DuplicateFailureDomain(_)
        | LocationError::AmbiguousAvailabilityDomain(_)
        | LocationError::DuplicateBinding { .. }
        | LocationError::StaleFailureDomainGeneration { .. }
        | LocationError::FailureDomainHasChildren(_)
        | LocationError::FailureDomainHasBindings(_)
        | LocationError::AvailabilityDomainHasFailureDomains(_)
        | LocationError::RegionHasAvailabilityDomains(_) => ErrorCode::Conflict,
        LocationError::UnknownFailureDomain(_)
        | LocationError::UnknownTopologyRegion(_)
        | LocationError::UnknownFailureDomainAvailabilityDomain(_, _)
        | LocationError::FailureDomainParentMissing(_, _) => ErrorCode::ResourceNotFound,
        _ => ErrorCode::BadRequest,
    }
}

/// Maps durable store failures to stable public error codes. Store reasons
/// (SQL, connection details) are logged, never sent.
fn store_error_code(error: &KernelError) -> ErrorCode {
    match error {
        KernelError::TopologyCorrupt(_) => ErrorCode::Conflict,
        KernelError::TopologyUnavailable(_) => ErrorCode::NotAvailable,
        _ => ErrorCode::InternalError,
    }
}

fn topology_problem(error: TopologyError) -> Response {
    let code = match &error {
        TopologyError::Location(location) => location_error_code(location),
        TopologyError::Store(store) => {
            tracing::warn!(%error, "topology store failure");
            store_error_code(store)
        }
    };
    ProblemDetails::new(code).into_response()
}

#[allow(clippy::result_large_err)]
fn require_guard(state: &NativeApiState) -> Result<Arc<TopologyGuard>, Response> {
    state
        .locations
        .clone()
        .ok_or_else(|| ProblemDetails::new(ErrorCode::NotAvailable).into_response())
}

#[allow(clippy::result_large_err)]
fn require_store(state: &NativeApiState) -> Result<Arc<dyn TopologyStore>, Response> {
    state
        .topology_store
        .clone()
        .ok_or_else(|| ProblemDetails::new(ErrorCode::NotAvailable).into_response())
}

/// Builds the mandatory durable audit event for one topology mutation request.
///
/// Topology mutations are operator-scope protection-domain changes: every
/// successful (2xx) mutation request is audited with the canonical audit
/// identity (principal, effective scope, request/audit correlation). The event
/// is folded into the SAME store transaction as the mutation (audit durability
/// is structural, P15.1 #931 MEDIUM-1), so there is no audit sink to configure
/// or fail-closed check — a missing sink is no longer possible. No-op replay
/// paths that produce no mutation still record the event via the store.
fn build_topology_audit_event(
    auth: &AuthContext,
    resource_kind: &str, // "failure_domain" | "region" | "availability_domain" | "binding"
    resource_id: Option<&str>,
) -> AuditEvent {
    AuditEvent::from_auth(
        auth,
        ServiceNamespace::new_unchecked("topology".to_owned()),
        manage_action(),
        AuditOutcome::Succeeded,
    )
    .with_resource(
        ResourceType::new_unchecked("topology", resource_kind),
        resource_id.map(ResourceId::new_unchecked),
        None,
    )
}

/// Converts a repository page (at most `limit + 1` rows) into the public
/// page contract: the extra row, when present, becomes the continuation.
#[allow(clippy::result_large_err)]
fn truncate_page<T>(
    mut items: Vec<T>,
    limit: usize,
    continuation: impl Fn(&T) -> Option<String>,
) -> Result<RepositoryPage<T>, Response> {
    let has_more = items.len() > limit;
    if has_more {
        items.truncate(limit);
    }
    let continuation_key = if has_more {
        items.last().and_then(&continuation)
    } else {
        None
    };
    RepositoryPage::new(items, has_more, continuation_key, limit)
        .map_err(|_| ProblemDetails::new(ErrorCode::InternalError).into_response())
}

// ── Failure domains ─────────────────────────────────────────────────────────

pub async fn list_failure_domains(
    auth: BearerAuth,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, read_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let validated = match state.cursor_config.validate_query(
        query.limit.as_deref(),
        query.cursor.as_deref(),
        auth.0.effective_scope().id().as_str(),
        "topology:failure_domain",
    ) {
        Ok(validated) => validated,
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let items = match store
        .list_failure_domains(validated.continuation_key(), validated.limit())
        .await
    {
        Ok(items) => items,
        Err(error) => return topology_problem(TopologyError::Store(error)),
    };
    // Fail closed rather than serve a malformed authoritative row: the store is
    // the durable authority, so a row that does not validate as well-formed
    // bedrock is a 503, not a document.
    for domain in &items {
        if let Err(error) = domain.validate_shape() {
            tracing::error!(%error, "stored failure domain failed shape validation; refusing to serve");
            return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
        }
    }
    let page = match truncate_page(items, validated.limit(), |domain| Some(domain.id.clone())) {
        Ok(page) => page,
        Err(response) => return response,
    };
    match state.cursor_config.complete_page(&validated, page) {
        Ok(page) => Json(page).into_response(),
        Err(_) => ProblemDetails::new(ErrorCode::InternalError).into_response(),
    }
}

/// Creates one failure domain. An identical replay (same id and content)
/// converges to `200 OK` with the current document; an id collision with
/// different content is a conflict.
pub async fn create_failure_domain(
    auth: BearerAuth,
    State(state): State<NativeApiState>,
    Json(body): Json<FailureDomainCreateRequest>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let class = match parse_class(&body.class) {
        Some(class) => class,
        None => {
            return ProblemDetails::with_detail(
                ErrorCode::BadRequest,
                "unknown failure domain class",
            )
            .into_response();
        }
    };
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    // The audit event is built up front and folded into the SAME store
    // transaction as the mutation (audit durability is structural).
    let audit_event = build_topology_audit_event(&auth.0, "failure_domain", Some(&body.id));
    // The registry owns generation identity: creations always start at 1.
    let requested = FailureDomain {
        id: body.id.clone(),
        class,
        name: body.name.clone(),
        availability_domain: body.availability_domain.clone(),
        parent: body.parent.clone(),
        generation: 1,
        metadata: body.metadata.clone(),
    };
    match guard
        .create_failure_domain(&*store, requested, Some(&audit_event))
        .await
    {
        Ok(()) => {
            let created = guard.failure_domain(&body.id).await;
            (StatusCode::CREATED, Json(serde_json::json!(created))).into_response()
        }
        Err(TopologyError::Location(LocationError::DuplicateFailureDomain(id))) => {
            // Replay convergence: an identical create returns the current
            // document; different content under the same id conflicts. A replay
            // is still an audited action — record the audit as a standalone
            // durable write (there is no mutation transaction to fold into).
            match guard.failure_domain(&id).await {
                Some(current)
                    if current.class == class
                        && current.name == body.name
                        && current.availability_domain == body.availability_domain
                        && current.parent == body.parent
                        && current.metadata == body.metadata =>
                {
                    if let Err(error) = store.record_audit(&audit_event).await {
                        tracing::error!(%error, "topology create-replay audit record failed");
                        return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
                    }
                    (StatusCode::OK, Json(serde_json::json!(current))).into_response()
                }
                _ => ProblemDetails::new(ErrorCode::Conflict).into_response(),
            }
        }
        Err(error) => topology_problem(error),
    }
}

pub async fn show_failure_domain(
    auth: BearerAuth,
    Path(id): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, read_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    match guard.failure_domain(&id).await {
        Some(domain) => Json(serde_json::json!(domain)).into_response(),
        None => ProblemDetails::not_found(Some(&id)).into_response(),
    }
}

pub async fn update_failure_domain(
    auth: BearerAuth,
    Path(id): Path<String>,
    headers: HeaderMap,
    State(state): State<NativeApiState>,
    Json(body): Json<FailureDomainUpdateRequest>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let Some(expected_generation) = parse_if_match_generation(&headers) else {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    };
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "failure_domain", Some(&id));
    match guard
        .update_failure_domain(
            &*store,
            &id,
            body.name,
            body.metadata,
            expected_generation,
            Some(&audit_event),
        )
        .await
    {
        Ok(()) => {
            let updated = guard.failure_domain(&id).await;
            (StatusCode::OK, Json(serde_json::json!(updated))).into_response()
        }
        Err(error) => topology_problem(error),
    }
}

pub async fn delete_failure_domain(
    auth: BearerAuth,
    Path(id): Path<String>,
    headers: HeaderMap,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let Some(expected_generation) = parse_if_match_generation(&headers) else {
        return ProblemDetails::new(ErrorCode::BadRequest).into_response();
    };
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "failure_domain", Some(&id));
    match guard
        .delete_failure_domain(&*store, &id, expected_generation, Some(&audit_event))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => topology_problem(error),
    }
}

// ── Bindings ────────────────────────────────────────────────────────────────

/// Separator for the encoded binding continuation key. Binding identity
/// components use the canonical location alphabet plus kebab-case kinds, so
/// this control character can never appear inside a component.
const BINDING_CURSOR_SEPARATOR: char = '\u{1f}';

fn binding_continuation_key(binding: &TopologyBinding) -> String {
    format!(
        "{}{}{}{}{}",
        binding.failure_domain,
        BINDING_CURSOR_SEPARATOR,
        binding.target.kind.as_str(),
        BINDING_CURSOR_SEPARATOR,
        binding.target.id
    )
}

fn parse_binding_continuation(key: &str) -> Option<BindingListCursor> {
    let (failure_domain, rest) = key.split_once(BINDING_CURSOR_SEPARATOR)?;
    let (target_kind, target_id) = rest.split_once(BINDING_CURSOR_SEPARATOR)?;
    if failure_domain.is_empty() || target_id.is_empty() || parse_kind(target_kind).is_none() {
        return None;
    }
    Some(BindingListCursor {
        failure_domain: failure_domain.to_owned(),
        target_kind: target_kind.to_owned(),
        target_id: target_id.to_owned(),
    })
}

pub async fn list_bindings(
    auth: BearerAuth,
    Path(id): Path<String>,
    Query(query): Query<PageQuery>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, read_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    if !guard.contains_failure_domain(&id).await {
        return ProblemDetails::not_found(Some(&id)).into_response();
    }
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    // Bind the failure-domain identity into the cursor so a continuation for
    // one domain's list can never be replayed against another's.
    let identity = format!("topology:bindings-of:{id}");
    let validated = match state.cursor_config.validate_query_with_identity(
        query.limit.as_deref(),
        query.cursor.as_deref(),
        auth.0.effective_scope().id().as_str(),
        "topology:binding",
        &identity,
    ) {
        Ok(validated) => validated,
        Err(_) => return ProblemDetails::new(ErrorCode::BadRequest).into_response(),
    };
    let after = match validated.continuation_key().map(parse_binding_continuation) {
        Some(Some(cursor)) => Some(cursor),
        Some(None) => return ProblemDetails::new(ErrorCode::InvalidCursor).into_response(),
        None => None,
    };
    let items = match store
        .list_bindings_of(&id, after.as_ref(), validated.limit())
        .await
    {
        Ok(items) => items,
        Err(error) => return topology_problem(TopologyError::Store(error)),
    };
    // Fail closed rather than serve a malformed authoritative binding.
    for binding in &items {
        if let Err(error) = binding.validate_shape() {
            tracing::error!(%error, "stored binding failed shape validation; refusing to serve");
            return ProblemDetails::new(ErrorCode::NotAvailable).into_response();
        }
    }
    let page = match truncate_page(items, validated.limit(), |binding| {
        Some(binding_continuation_key(binding))
    }) {
        Ok(page) => page,
        Err(response) => return response,
    };
    match state.cursor_config.complete_page(&validated, page) {
        Ok(page) => Json(page).into_response(),
        Err(_) => ProblemDetails::new(ErrorCode::InternalError).into_response(),
    }
}

pub async fn bind(
    auth: BearerAuth,
    Path((id, kind, target)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    if !guard.contains_failure_domain(&id).await {
        return ProblemDetails::not_found(Some(&id)).into_response();
    }
    let Some(kind) = parse_kind(&kind) else {
        return ProblemDetails::with_detail(ErrorCode::BadRequest, "unknown binding target kind")
            .into_response();
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "binding", Some(&id));
    let binding = TopologyBinding {
        failure_domain: id.clone(),
        target: o3k_kernel::BindingTarget { kind, id: target },
    };
    match guard
        .bind(&*store, binding.clone(), Some(&audit_event))
        .await
    {
        // Binding is replay-idempotent in the kernel: an existing binding
        // converges to 200 with the binding document (still audited).
        Ok(()) => (StatusCode::OK, Json(serde_json::json!(binding))).into_response(),
        Err(error) => topology_problem(error),
    }
}

pub async fn unbind(
    auth: BearerAuth,
    Path((id, kind, target)): Path<(String, String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), instance_target(&id)) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    if !guard.contains_failure_domain(&id).await {
        return ProblemDetails::not_found(Some(&id)).into_response();
    }
    let Some(kind) = parse_kind(&kind) else {
        return ProblemDetails::with_detail(ErrorCode::BadRequest, "unknown binding target kind")
            .into_response();
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "binding", Some(&id));
    let binding = TopologyBinding {
        failure_domain: id.clone(),
        target: o3k_kernel::BindingTarget { kind, id: target },
    };
    match guard.unbind(&*store, binding, Some(&audit_event)).await {
        // Unbind is replay-idempotent: an absent binding converges to 204
        // (still audited).
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => topology_problem(error),
    }
}

// ── Regions and availability domains ────────────────────────────────────────

/// Declares one canonical region idempotently. Both the first create and an
/// identical replay return `200 OK` with the region document (replay
/// convergence).
pub async fn declare_region(
    auth: BearerAuth,
    Path(region): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "region", Some(&region));
    match guard
        .declare_region(&*store, &region, Some(&audit_event))
        .await
    {
        Ok(()) => {
            let document = guard.region(&region).await;
            (StatusCode::OK, Json(serde_json::json!(document))).into_response()
        }
        Err(error) => topology_problem(error),
    }
}

/// Removes one canonical region. Removing an absent region is an idempotent
/// no-op (204); a region that still has availability domains conflicts.
pub async fn remove_region(
    auth: BearerAuth,
    Path(region): Path<String>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "region", Some(&region));
    match guard
        .remove_region(&*store, &region, Some(&audit_event))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => topology_problem(error),
    }
}

/// Declares one canonical availability domain inside `region` idempotently.
/// Returns `200 OK` with the owning region document.
pub async fn declare_availability_domain(
    auth: BearerAuth,
    Path((region, az)): Path<(String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    let audit_event = build_topology_audit_event(&auth.0, "availability_domain", Some(&az));
    match guard
        .declare_availability_domain(&*store, &region, &az, Some(&audit_event))
        .await
    {
        Ok(()) => {
            let document = guard.region(&region).await;
            (StatusCode::OK, Json(serde_json::json!(document))).into_response()
        }
        Err(error) => topology_problem(error),
    }
}

/// Removes one canonical availability domain. The route identifies the
/// owning region: an unknown region or an az the region does not declare is
/// a 404; an az that still has failure domains conflicts.
pub async fn remove_availability_domain(
    auth: BearerAuth,
    Path((region, az)): Path<(String, String)>,
    State(state): State<NativeApiState>,
) -> Response {
    if !authorize(&state, &auth.0, manage_action(), collection_target()) {
        return ProblemDetails::new(ErrorCode::Forbidden).into_response();
    }
    let guard = match require_guard(&state) {
        Ok(guard) => guard,
        Err(response) => return response,
    };
    let store = match require_store(&state) {
        Ok(store) => store,
        Err(response) => return response,
    };
    match guard.region(&region).await {
        None => return ProblemDetails::not_found(Some(&region)).into_response(),
        Some(document)
            if !document
                .availability_domains
                .iter()
                .any(|known| known.id == az) =>
        {
            return ProblemDetails::not_found(Some(&az)).into_response();
        }
        Some(_) => {}
    }
    let audit_event = build_topology_audit_event(&auth.0, "availability_domain", Some(&az));
    match guard
        .remove_availability_domain(&*store, &az, Some(&audit_event))
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => topology_problem(error),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::auth;
    use crate::pagination;
    use axum::body::Body;
    use axum::http::Request;
    use o3k_kernel::{
        AuthContext, OwnershipScope, Principal, PrincipalId, ScopeId, ScopeKind, ServicePrincipal,
        UserPrincipal,
    };
    use std::collections::{BTreeMap, BTreeSet};
    use std::sync::Mutex;

    // ── Test doubles ─────────────────────────────────────────────────────

    #[derive(Clone)]
    struct TestIssuer(AuthContext);

    #[async_trait::async_trait]
    impl auth::TokenIssuer for TestIssuer {
        async fn issue_native(
            &self,
            _request: &auth::NativeTokenRequestV1,
        ) -> Result<(String, serde_json::Value), error::ProblemDetails> {
            Err(error::ProblemDetails::unauthorized())
        }

        async fn auth_context(&self, _token: &str) -> Result<AuthContext, error::ProblemDetails> {
            Ok(self.0.clone())
        }
    }

    use crate::error;

    /// In-memory [`TopologyStore`] double with the same uniqueness,
    /// referential-integrity, generation, and bounded-page semantics the real
    /// adapters enforce (mirrors the kernel test double).
    #[derive(Debug, Default)]
    struct MemoryTopologyStore {
        state: Mutex<MemoryState>,
    }

    #[derive(Debug, Default)]
    struct MemoryState {
        regions: BTreeSet<String>,
        az_to_region: BTreeMap<String, String>,
        failure_domains: BTreeMap<String, FailureDomain>,
        bindings: BTreeSet<(String, BindingTargetKind, String)>,
        audit_events: Vec<AuditEvent>,
    }

    impl MemoryTopologyStore {
        fn state(&self) -> std::sync::MutexGuard<'_, MemoryState> {
            self.state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
        }

        /// Pushes the caller's audit event onto an already-held store-state guard.
        /// The mutation methods already hold the lock, so we must never
        /// re-acquire it here (a `std::sync::Mutex` is not reentrant).
        fn push_audit(state: &mut MemoryState, audit: Option<&AuditEvent>) {
            if let Some(audit) = audit {
                state.audit_events.push(audit.clone());
            }
        }
    }

    #[async_trait::async_trait]
    impl TopologyStore for MemoryTopologyStore {
        async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
            let state = self.state();
            let mut regions: BTreeMap<String, RegionDeclaration> = BTreeMap::new();
            for region_id in &state.regions {
                regions.insert(
                    region_id.clone(),
                    RegionDeclaration {
                        id: region_id.clone(),
                        availability_domains: Vec::new(),
                    },
                );
            }
            for (az, region) in &state.az_to_region {
                regions
                    .entry(region.clone())
                    .or_insert_with(|| RegionDeclaration {
                        id: region.clone(),
                        availability_domains: Vec::new(),
                    })
                    .availability_domains
                    .push(o3k_kernel::AvailabilityDomain { id: az.clone() });
            }
            let bindings = state
                .bindings
                .iter()
                .map(|(fd, kind, id)| TopologyBinding {
                    failure_domain: fd.clone(),
                    target: o3k_kernel::BindingTarget {
                        kind: *kind,
                        id: id.clone(),
                    },
                })
                .collect();
            Ok(TopologySnapshot {
                regions: regions.into_values().collect(),
                failure_domains: state.failure_domains.values().cloned().collect(),
                bindings,
            })
        }

        async fn insert_region(
            &self,
            region_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.regions.insert(region_id.to_owned()) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate region '{region_id}'"
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_region(
            &self,
            region_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if state
                .az_to_region
                .values()
                .any(|region| region == region_id)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "region '{region_id}' still has availability domains"
                )));
            }
            state.regions.remove(region_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_availability_domain(
            &self,
            region_id: &str,
            az_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.regions.contains(region_id) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "unknown region '{region_id}'"
                )));
            }
            if state
                .az_to_region
                .insert(az_id.to_owned(), region_id.to_owned())
                .is_some()
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate availability domain '{az_id}'"
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_availability_domain(
            &self,
            az_id: &str,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if state
                .failure_domains
                .values()
                .any(|domain| domain.availability_domain == az_id)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "availability domain '{az_id}' still has failure domains"
                )));
            }
            state.az_to_region.remove(az_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_failure_domain(
            &self,
            domain: &FailureDomain,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.az_to_region.contains_key(&domain.availability_domain) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "failure domain '{}' references unknown availability domain '{}'",
                    domain.id, domain.availability_domain
                )));
            }
            if let Some(parent) = &domain.parent
                && !state.failure_domains.contains_key(parent)
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "failure domain '{}' has dangling parent '{parent}'",
                    domain.id
                )));
            }
            if state
                .failure_domains
                .insert(domain.id.clone(), domain.clone())
                .is_some()
            {
                return Err(KernelError::TopologyCorrupt(format!(
                    "duplicate failure domain '{}'",
                    domain.id
                )));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn update_failure_domain(
            &self,
            domain: &FailureDomain,
            expected_generation: u64,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            let stored = state.failure_domains.get(&domain.id).ok_or_else(|| {
                KernelError::TopologyCorrupt(format!("unknown failure domain '{}'", domain.id))
            })?;
            if stored.generation != expected_generation {
                return Err(KernelError::TopologyCorrupt(format!(
                    "stale generation for failure domain '{}' (expected {expected_generation})",
                    domain.id
                )));
            }
            state
                .failure_domains
                .insert(domain.id.clone(), domain.clone());
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_failure_domain(
            &self,
            domain_id: &str,
            expected_generation: u64,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            let stored = state.failure_domains.get(domain_id).ok_or_else(|| {
                KernelError::TopologyCorrupt(format!("unknown failure domain '{domain_id}'"))
            })?;
            if stored.generation != expected_generation {
                return Err(KernelError::TopologyCorrupt(format!(
                    "stale generation for failure domain '{domain_id}' (expected {expected_generation})"
                )));
            }
            state.failure_domains.remove(domain_id);
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn insert_binding(
            &self,
            binding: &TopologyBinding,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            if !state.failure_domains.contains_key(&binding.failure_domain) {
                return Err(KernelError::TopologyCorrupt(format!(
                    "binding references unknown failure domain '{}'",
                    binding.failure_domain
                )));
            }
            if !state.bindings.insert((
                binding.failure_domain.clone(),
                binding.target.kind,
                binding.target.id.clone(),
            )) {
                return Err(KernelError::TopologyCorrupt("duplicate binding".to_owned()));
            }
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn delete_binding(
            &self,
            binding: &TopologyBinding,
            audit: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            let mut state = self.state();
            state.bindings.remove(&(
                binding.failure_domain.clone(),
                binding.target.kind,
                binding.target.id.clone(),
            ));
            Self::push_audit(&mut state, audit);
            Ok(())
        }

        async fn list_failure_domains(
            &self,
            after_id: Option<&str>,
            limit: usize,
        ) -> Result<Vec<FailureDomain>, KernelError> {
            let state = self.state();
            Ok(state
                .failure_domains
                .values()
                .filter(|domain| after_id.is_none_or(|after| domain.id.as_str() > after))
                .take(limit.saturating_add(1))
                .cloned()
                .collect())
        }

        async fn list_bindings(
            &self,
            after: Option<&BindingListCursor>,
            limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            let state = self.state();
            Ok(state
                .bindings
                .iter()
                .filter(|(fd, kind, id)| {
                    after.is_none_or(|cursor| {
                        (fd.as_str(), kind.as_str(), id.as_str())
                            > (
                                cursor.failure_domain.as_str(),
                                cursor.target_kind.as_str(),
                                cursor.target_id.as_str(),
                            )
                    })
                })
                .take(limit.saturating_add(1))
                .map(|(fd, kind, id)| TopologyBinding {
                    failure_domain: fd.clone(),
                    target: o3k_kernel::BindingTarget {
                        kind: *kind,
                        id: id.clone(),
                    },
                })
                .collect())
        }

        async fn list_bindings_of(
            &self,
            failure_domain: &str,
            after: Option<&BindingListCursor>,
            limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            let state = self.state();
            Ok(state
                .bindings
                .iter()
                .filter(|(fd, _, _)| fd == failure_domain)
                .filter(|(_, kind, id)| {
                    after.is_none_or(|cursor| {
                        (kind.as_str(), id.as_str())
                            > (cursor.target_kind.as_str(), cursor.target_id.as_str())
                    })
                })
                .take(limit.saturating_add(1))
                .map(|(fd, kind, id)| TopologyBinding {
                    failure_domain: fd.clone(),
                    target: o3k_kernel::BindingTarget {
                        kind: *kind,
                        id: id.clone(),
                    },
                })
                .collect())
        }

        async fn record_audit(&self, audit: &AuditEvent) -> Result<(), KernelError> {
            Self::push_audit(&mut self.state(), Some(audit));
            Ok(())
        }
    }

    /// Store double whose read path yields one malformed failure domain shape,
    /// to prove the API read path fails closed (503) instead of serving it.
    #[derive(Default)]
    struct MalformedRowTopologyStore {
        state: Mutex<MemoryState>,
    }

    impl MalformedRowTopologyStore {
        /// A failure domain whose display name exceeds the shape bound, so the
        /// read path must fail closed rather than serve it.
        fn malformed_domain() -> FailureDomain {
            let mut fd = FailureDomain {
                id: "rack-1".to_owned(),
                class: FailureDomainClass::Rack,
                name: "Rack".to_owned(),
                availability_domain: "az-1".to_owned(),
                parent: None,
                generation: 1,
                metadata: BTreeMap::new(),
            };
            fd.name = "x".repeat(257);
            fd
        }
    }

    #[async_trait::async_trait]
    impl TopologyStore for MalformedRowTopologyStore {
        async fn load_snapshot(&self) -> Result<TopologySnapshot, KernelError> {
            let _state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            Ok(TopologySnapshot {
                regions: Vec::new(),
                failure_domains: vec![Self::malformed_domain()],
                bindings: Vec::new(),
            })
        }
        async fn insert_region(
            &self,
            _r: &str,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn delete_region(
            &self,
            _r: &str,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn insert_availability_domain(
            &self,
            _r: &str,
            _az: &str,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn delete_availability_domain(
            &self,
            _az: &str,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn insert_failure_domain(
            &self,
            _d: &FailureDomain,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn update_failure_domain(
            &self,
            _d: &FailureDomain,
            _g: u64,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn delete_failure_domain(
            &self,
            _d: &str,
            _g: u64,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn insert_binding(
            &self,
            _b: &TopologyBinding,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn delete_binding(
            &self,
            _b: &TopologyBinding,
            _a: Option<&AuditEvent>,
        ) -> Result<(), KernelError> {
            Ok(())
        }
        async fn list_failure_domains(
            &self,
            _after: Option<&str>,
            _limit: usize,
        ) -> Result<Vec<FailureDomain>, KernelError> {
            let _state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            Ok(vec![Self::malformed_domain()])
        }
        async fn list_bindings(
            &self,
            _after: Option<&BindingListCursor>,
            _limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            Ok(Vec::new())
        }
        async fn list_bindings_of(
            &self,
            _fd: &str,
            _after: Option<&BindingListCursor>,
            _limit: usize,
        ) -> Result<Vec<TopologyBinding>, KernelError> {
            Ok(Vec::new())
        }
        async fn record_audit(&self, _a: &AuditEvent) -> Result<(), KernelError> {
            Ok(())
        }
    }

    // ── Contexts ─────────────────────────────────────────────────────────

    fn system_scope() -> OwnershipScope {
        OwnershipScope::new(
            ScopeId::new_unchecked("system"),
            ScopeKind::System,
            None,
            None,
        )
    }

    fn project_scope() -> OwnershipScope {
        OwnershipScope::new(
            ScopeId::new_unchecked("project-a"),
            ScopeKind::Project,
            None,
            None,
        )
    }

    fn operator_context() -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked("operator-1"),
                "operator",
                None,
            )),
            system_scope(),
            vec!["operator".to_owned()],
            1,
            2,
            "audit-topology",
            "request-topology",
            None,
        )
    }

    fn tenant_context() -> AuthContext {
        AuthContext::new(
            Principal::User(UserPrincipal::new(
                PrincipalId::new_unchecked("user-1"),
                "member",
                None,
            )),
            project_scope(),
            vec!["member".to_owned()],
            1,
            2,
            "audit-tenant",
            "request-tenant",
            None,
        )
    }

    /// Provider-class principal: a service principal carrying system scope
    /// but no durable operator role.
    fn provider_context() -> AuthContext {
        AuthContext::new(
            Principal::Service(ServicePrincipal::new(
                PrincipalId::new_unchecked("agent-1"),
                "compute-agent",
                "compute-agent",
            )),
            system_scope(),
            vec![],
            1,
            2,
            "audit-agent",
            "request-agent",
            None,
        )
    }

    // ── Composition helpers ──────────────────────────────────────────────

    const CURSOR_KEY: &[u8] = b"topology-test-cursor-key-0123456789";

    async fn topology_fixture() -> (Arc<MemoryTopologyStore>, Arc<TopologyGuard>) {
        let store = Arc::new(MemoryTopologyStore::default());
        let mut registry = LocationRegistry::default();
        // Seeding direct through the registry, not an HTTP mutation request.
        registry
            .declare_region(&*store, "region-a", None)
            .await
            .unwrap();
        registry
            .declare_availability_domain(&*store, "region-a", "az-1", None)
            .await
            .unwrap();
        registry
            .declare_availability_domain(&*store, "region-a", "az-2", None)
            .await
            .unwrap();
        (store, Arc::new(TopologyGuard::new(registry)))
    }

    fn topology_api_state(
        context: AuthContext,
        store: Arc<MemoryTopologyStore>,
        guard: Arc<TopologyGuard>,
    ) -> NativeApiState {
        NativeApiState::new(
            None,
            pagination::CursorConfig::new(CURSOR_KEY.to_vec()).unwrap(),
            Some(Arc::new(TestIssuer(context))),
            None,
            None,
            None,
        )
        .unwrap()
        .with_locations(guard)
        .with_topology_store(store)
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()))
    }

    /// Production-like state plus the durable store so tests can inspect the
    /// transaction-atomic audit rows. Audit is unconditional inside the store
    /// mutation (MEDIUM-1), so there is no separate sink.
    fn topology_api_state_with_store(
        context: AuthContext,
        store: Arc<MemoryTopologyStore>,
        guard: Arc<TopologyGuard>,
    ) -> (NativeApiState, Arc<MemoryTopologyStore>) {
        (topology_api_state(context, store.clone(), guard), store)
    }

    async fn topology_state(context: AuthContext) -> (NativeApiState, Arc<MemoryTopologyStore>) {
        let (store, guard) = topology_fixture().await;
        topology_api_state_with_store(context, store, guard)
    }

    async fn topology_state_with_store(
        context: AuthContext,
    ) -> (NativeApiState, Arc<MemoryTopologyStore>) {
        let (store, guard) = topology_fixture().await;
        topology_api_state_with_store(context, store, guard)
    }

    fn fd_request(id: &str, class: &str, az: &str) -> serde_json::Value {
        serde_json::json!({
            "id": id,
            "class": class,
            "name": format!("{id} display"),
            "availability_domain": az,
        })
    }

    async fn call(
        state: NativeApiState,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
        if_match: Option<&str>,
    ) -> (StatusCode, serde_json::Value) {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", "Bearer test-token");
        if let Some(generation) = if_match {
            builder = builder.header("if-match", generation);
        }
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&value).unwrap()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = axum::response::Response::from(
            tower::ServiceExt::oneshot(crate::router(state), request)
                .await
                .unwrap(),
        );
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body = if bytes.is_empty() {
            serde_json::Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null)
        };
        (status, body)
    }

    async fn create_failure_domain(
        state: NativeApiState,
        id: &str,
        az: &str,
    ) -> (StatusCode, serde_json::Value) {
        call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(fd_request(id, "rack", az)),
            None,
        )
        .await
    }

    fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
        let mut keys: Vec<String> = value.as_object().expect("object").keys().cloned().collect();
        keys.sort();
        keys
    }

    // ── CRUD happy paths ─────────────────────────────────────────────────

    #[tokio::test]
    async fn failure_domain_crud_happy_path() {
        let (state, store) = topology_state(operator_context()).await;

        let (status, created) = create_failure_domain(state.clone(), "rack-1", "az-1").await;
        assert_eq!(status, StatusCode::CREATED);
        crate::assert_topology_failure_domain_schema(&created);
        assert_eq!(
            sorted_keys(&created),
            vec![
                "availability_domain",
                "class",
                "generation",
                "id",
                "metadata",
                "name",
                "parent"
            ]
        );
        assert_eq!(created["id"], "rack-1");
        assert_eq!(created["class"], "rack");
        assert_eq!(created["generation"], 1);
        assert_eq!(created["parent"], serde_json::Value::Null);

        // The durable store and in-memory authority agree.
        let (status, shown) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains/rack-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(shown, created);
        let guard_snapshot = state.locations.as_ref().unwrap().snapshot().await;
        let durable = store.load_snapshot().await.unwrap();
        assert_eq!(guard_snapshot, durable);

        // Update with a valid precondition bumps the generation.
        let (status, updated) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1",
            Some(serde_json::json!({"name": "renamed rack", "metadata": {"aisle": "7"}})),
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_topology_failure_domain_schema(&updated);
        assert_eq!(updated["name"], "renamed rack");
        assert_eq!(updated["generation"], 2);
        assert_eq!(updated["metadata"], serde_json::json!({"aisle": "7"}));

        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/rack-1",
            None,
            Some("generation-2"),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);

        let (status, problem) =
            call(state, "GET", "/topology/failure-domains/rack-1", None, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(problem["code"], "RESOURCE_NOT_FOUND");
    }

    // ── Replay idempotency ───────────────────────────────────────────────

    #[tokio::test]
    async fn duplicate_create_with_identical_body_replays_to_200() {
        let (state, _) = topology_state(operator_context()).await;
        let (status, created) = create_failure_domain(state.clone(), "rack-1", "az-1").await;
        assert_eq!(status, StatusCode::CREATED);

        // Identical replay converges to 200 with the current document.
        let (status, replayed) = create_failure_domain(state.clone(), "rack-1", "az-1").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed, created);

        // The same id with different content is a conflict.
        let (status, problem) = call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(serde_json::json!({
                "id": "rack-1",
                "class": "rack",
                "name": "a different rack",
                "availability_domain": "az-1",
            })),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["code"], "CONFLICT");
    }

    #[tokio::test]
    async fn region_and_az_declares_are_replay_idempotent() {
        let (state, _) = topology_state(operator_context()).await;

        let (status, region) = call(state.clone(), "PUT", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sorted_keys(&region), vec!["availability_domains", "id"]);
        assert_eq!(region["id"], "region-b");
        assert_eq!(region["availability_domains"], serde_json::json!([]));

        // Replay returns the same document with the same status.
        let (status, replayed) = call(state.clone(), "PUT", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed, region);

        let (status, with_az) = call(
            state.clone(),
            "PUT",
            "/regions/region-b/availability-domains/az-3",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            with_az["availability_domains"],
            serde_json::json!([{"id": "az-3"}])
        );

        let (status, replayed) = call(
            state.clone(),
            "PUT",
            "/regions/region-b/availability-domains/az-3",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(replayed, with_az);

        // Discovery reflects the mutation with the exact contract shape.
        let (status, discovery) = call(state.clone(), "GET", "/regions", None, None).await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_location_discovery_schema(&discovery);
        assert_eq!(discovery["count"], 2);

        // Teardown in dependency order converges.
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/regions/region-b/availability-domains/az-3",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(state, "DELETE", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn bindings_are_replay_idempotent() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-1", "az-1").await;

        let (status, bound) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_topology_binding_schema(&bound);
        assert_eq!(sorted_keys(&bound), vec!["failure_domain", "target"]);
        assert_eq!(bound["failure_domain"], "rack-1");
        assert_eq!(
            bound["target"],
            serde_json::json!({"kind": "host", "id": "hv-1"})
        );

        // Re-binding the same target converges to 200 with the document.
        let (status, rebound) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(rebound, bound);

        // Unbind is idempotent: absent binding still converges to 204.
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            state,
            "DELETE",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    // ── Conflicts ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn update_and_delete_require_valid_if_match() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-1", "az-1").await;

        // Missing precondition.
        let (status, problem) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1",
            Some(serde_json::json!({"name": "renamed", "metadata": {}})),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["code"], "BAD_REQUEST");

        // Malformed precondition.
        let (status, _) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1",
            Some(serde_json::json!({"name": "renamed", "metadata": {}})),
            Some("bogus"),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Stale precondition.
        let (status, problem) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1",
            Some(serde_json::json!({"name": "renamed", "metadata": {}})),
            Some("generation-9"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["code"], "CONFLICT");

        let (status, _) = call(
            state,
            "DELETE",
            "/topology/failure-domains/rack-1",
            None,
            Some("generation-9"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn delete_protections_surface_conflicts() {
        let (state, _) = topology_state(operator_context()).await;
        call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(fd_request("building-1", "building", "az-1")),
            None,
        )
        .await;
        let mut child = fd_request("rack-1", "rack", "az-1");
        child["parent"] = serde_json::json!("building-1");
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(child),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        // A parent with children cannot be deleted.
        let (status, problem) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/building-1",
            None,
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(problem["code"], "CONFLICT");

        // Nor can a domain that still has bindings.
        call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/rack-1",
            None,
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Bottom-up teardown works.
        call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/rack-1",
            None,
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(
            state,
            "DELETE",
            "/topology/failure-domains/building-1",
            None,
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn region_and_az_delete_protections_surface_conflicts() {
        let (state, _) = topology_state(operator_context()).await;

        // A region with availability domains cannot be removed.
        let (status, _) = call(state.clone(), "DELETE", "/regions/region-a", None, None).await;
        assert_eq!(status, StatusCode::CONFLICT);

        // An availability domain with failure domains cannot be removed.
        create_failure_domain(state.clone(), "rack-1", "az-1").await;
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/regions/region-a/availability-domains/az-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CONFLICT);

        // Removing the region itself is rejected until every AZ is gone.
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/regions/region-a/availability-domains/az-2",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NO_CONTENT);
        let (status, _) = call(state.clone(), "DELETE", "/regions/region-a", None, None).await;
        assert_eq!(status, StatusCode::CONFLICT);
    }

    // ── Validation failures ──────────────────────────────────────────────

    #[tokio::test]
    async fn create_validation_failures_are_400() {
        let (state, _) = topology_state(operator_context()).await;

        // Malformed id.
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(fd_request("Bad ID", "rack", "az-1")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Unknown class.
        let (status, problem) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(fd_request("rack-1", "moon", "az-1")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["code"], "BAD_REQUEST");

        // Name bounds.
        let mut empty_name = fd_request("rack-1", "rack", "az-1");
        empty_name["name"] = serde_json::json!("");
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(empty_name),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let mut long_name = fd_request("rack-1", "rack", "az-1");
        long_name["name"] = serde_json::json!("x".repeat(257));
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(long_name),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Metadata bounds: entry count, key alphabet, value length.
        let mut too_many = fd_request("rack-1", "rack", "az-1");
        too_many["metadata"] = serde_json::json!(
            (0..=32)
                .map(|i| (format!("k{i:02}"), "v".to_owned()))
                .collect::<BTreeMap<_, _>>()
        );
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(too_many),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let mut bad_key = fd_request("rack-1", "rack", "az-1");
        bad_key["metadata"] = serde_json::json!({"Bad Key": "v"});
        let (status, _) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(bad_key),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        let mut long_value = fd_request("rack-1", "rack", "az-1");
        long_value["metadata"] = serde_json::json!({"note": "x".repeat(257)});
        let (status, _) = call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(long_value),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn nesting_rules_are_enforced() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-az-1", "az-1").await;

        // Cross-availability-domain parents are rejected via the kernel rule.
        let mut cross_az = fd_request("rack-az-2", "rack", "az-2");
        cross_az["parent"] = serde_json::json!("rack-az-1");
        let (status, problem) = call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(cross_az),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["code"], "BAD_REQUEST");
    }

    #[tokio::test]
    async fn binding_kind_and_target_are_validated() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-1", "az-1").await;

        // Unknown kind.
        let (status, problem) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1/bindings/moon/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(problem["code"], "BAD_REQUEST");

        // Malformed target id.
        let (status, _) = call(
            state,
            "PUT",
            "/topology/failure-domains/rack-1/bindings/host/Bad%20ID",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── Not found ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn absent_topology_is_not_found() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-1", "az-1").await;

        let (status, problem) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains/ghost",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(problem["code"], "RESOURCE_NOT_FOUND");
        assert_eq!(problem["resource_id"], "ghost");

        let (status, _) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/ghost",
            Some(serde_json::json!({"name": "x", "metadata": {}})),
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/ghost",
            None,
            Some("generation-1"),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Bindings of an absent failure domain are not found.
        let (status, _) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains/ghost/bindings",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/ghost/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/topology/failure-domains/ghost/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Availability domains belong to regions; unknown region or an az the
        // region does not declare are not found.
        let (status, _) = call(
            state.clone(),
            "PUT",
            "/regions/ghost-region/availability-domains/az-9",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            state.clone(),
            "DELETE",
            "/regions/region-a/availability-domains/az-9",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
        let (status, _) = call(
            state,
            "DELETE",
            "/regions/ghost-region/availability-domains/az-9",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }

    // ── Authorization ────────────────────────────────────────────────────

    #[tokio::test]
    async fn missing_token_is_401() {
        let (state, _) = topology_state(operator_context()).await;
        let app = crate::router(state);
        for (method, uri) in [
            ("GET", "/topology/failure-domains"),
            ("POST", "/topology/failure-domains"),
            ("PUT", "/regions/region-b"),
        ] {
            let request = Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap();
            let response = axum::response::Response::from(
                tower::ServiceExt::oneshot(app.clone(), request)
                    .await
                    .unwrap(),
            );
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri}"
            );
            assert_eq!(
                response
                    .headers()
                    .get("Content-Type")
                    .unwrap()
                    .to_str()
                    .unwrap(),
                "application/problem+json"
            );
        }
    }

    #[tokio::test]
    async fn tenant_tokens_read_but_never_mutate() {
        let (store, guard) = topology_fixture().await;
        let operator_state = topology_api_state(operator_context(), store.clone(), guard.clone());
        create_failure_domain(operator_state.clone(), "rack-1", "az-1").await;

        let tenant_state = topology_api_state(tenant_context(), store, guard);
        let (status, body) = call(
            tenant_state.clone(),
            "GET",
            "/topology/failure-domains",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_topology_failure_domain_schema(&body["items"][0]);
        let (status, _) = call(
            tenant_state.clone(),
            "GET",
            "/topology/failure-domains/rack-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        for (method, uri, body) in [
            (
                "POST",
                "/topology/failure-domains",
                Some(fd_request("rack-9", "rack", "az-1")),
            ),
            (
                "PUT",
                "/topology/failure-domains/rack-1",
                Some(serde_json::json!({"name": "x", "metadata": {}})),
            ),
            ("DELETE", "/topology/failure-domains/rack-1", None),
            ("PUT", "/regions/region-b", None),
            ("DELETE", "/regions/region-a", None),
            (
                "PUT",
                "/topology/failure-domains/rack-1/bindings/host/hv-9",
                None,
            ),
        ] {
            let (status, problem) = call(
                tenant_state.clone(),
                method,
                uri,
                body,
                Some("generation-1"),
            )
            .await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{method} {uri}");
            assert_eq!(problem["code"], "FORBIDDEN", "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn provider_class_principal_cannot_manage_topology() {
        let (state, _) = topology_state(provider_context()).await;
        let (status, problem) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(fd_request("rack-1", "rack", "az-1")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(problem["code"], "FORBIDDEN");

        let (status, _) = call(state, "PUT", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn missing_authorizer_fails_closed() {
        let (state, store) = topology_state(operator_context()).await;
        let state = NativeApiState::new(
            None,
            pagination::CursorConfig::new(CURSOR_KEY.to_vec()).unwrap(),
            state.token_issuer.clone(),
            None,
            None,
            None,
        )
        .unwrap()
        .with_locations(state.locations.clone().unwrap())
        .with_topology_store(store);
        let (status, problem) = call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(fd_request("rack-1", "rack", "az-1")),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert_eq!(problem["code"], "FORBIDDEN");
    }

    // ── Pagination ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn failure_domain_listing_is_bounded_and_keyset_paged() {
        let (state, _) = topology_state(operator_context()).await;
        for id in ["rack-1", "rack-2", "rack-3"] {
            create_failure_domain(state.clone(), id, "az-1").await;
        }

        let (status, page) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains?limit=2",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sorted_keys(&page), vec!["has_more", "items", "next_cursor"]);
        crate::assert_topology_failure_domain_schema(&page);
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], true);
        let cursor = page["next_cursor"].as_str().unwrap().to_owned();

        // The continuation starts strictly after the last seen id.
        let first_ids: Vec<String> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect();
        let (status, page) = call(
            state.clone(),
            "GET",
            &format!("/topology/failure-domains?limit=2&cursor={cursor}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["has_more"], false);
        assert!(page.get("next_cursor").is_none());
        let remaining: Vec<String> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["id"].as_str().unwrap().to_owned())
            .collect();
        assert!(remaining.iter().all(|id| !first_ids.contains(id)));

        // Rejects over-limit bounds, empty bounds, and unknown parameters.
        for uri in [
            "/topology/failure-domains?limit=201",
            "/topology/failure-domains?limit=0",
            "/topology/failure-domains?bogus=value",
        ] {
            let (status, _) = call(state.clone(), "GET", uri, None, None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}");
        }

        // Tampered cursors fail closed.
        let mut tampered = cursor.clone();
        tampered.replace_range(0..1, "x");
        let (status, _) = call(
            state,
            "GET",
            &format!("/topology/failure-domains?cursor={tampered}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn binding_listing_is_bounded_and_bound_to_its_failure_domain() {
        let (state, _) = topology_state(operator_context()).await;
        create_failure_domain(state.clone(), "rack-1", "az-1").await;
        create_failure_domain(state.clone(), "rack-2", "az-1").await;
        for target in ["hv-1", "hv-2", "hv-3"] {
            call(
                state.clone(),
                "PUT",
                &format!("/topology/failure-domains/rack-1/bindings/host/{target}"),
                None,
                None,
            )
            .await;
        }

        let (status, page) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains/rack-1/bindings?limit=2",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        crate::assert_topology_binding_schema(&page);
        assert_eq!(page["items"].as_array().unwrap().len(), 2);
        assert_eq!(page["has_more"], true);
        let cursor = page["next_cursor"].as_str().unwrap().to_owned();

        let (status, page) = call(
            state.clone(),
            "GET",
            &format!("/topology/failure-domains/rack-1/bindings?limit=2&cursor={cursor}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"].as_array().unwrap().len(), 1);
        assert_eq!(page["has_more"], false);

        // rack-2 has no bindings; an empty first page has no continuation.
        let (status, page) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains/rack-2/bindings",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(page["items"].as_array().unwrap().len(), 0);
        assert_eq!(page["has_more"], false);

        // A continuation minted for rack-1 cannot be replayed against rack-2:
        // the cursor is bound to the failure-domain list identity.
        let (status, _) = call(
            state,
            "GET",
            &format!("/topology/failure-domains/rack-2/bindings?cursor={cursor}"),
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    // ── Response hygiene ─────────────────────────────────────────────────

    #[tokio::test]
    async fn topology_responses_expose_no_internal_fields() {
        let (state, _) = topology_state(operator_context()).await;
        let mut request = fd_request("rack-1", "rack", "az-1");
        request["metadata"] = serde_json::json!({"aisle": "7"});
        let (status, created) = call(
            state.clone(),
            "POST",
            "/topology/failure-domains",
            Some(request),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::CREATED);

        let serialized = serde_json::to_string(&created).unwrap();
        for leaked in ["RwLock", "Mutex", "pool", "sql", "connection"] {
            assert!(
                !serialized.to_lowercase().contains(&leaked.to_lowercase()),
                "internal token {leaked} leaked into the failure domain document"
            );
        }

        // Region documents carry exactly the discovery contract keys.
        let (status, region) = call(state, "PUT", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(sorted_keys(&region), vec!["availability_domains", "id"]);
    }

    // ── Mandatory durable audit (P15.1, issue #931, MEDIUM-1) ─────────────
    //
    // Audit is structural: it is folded into the SAME store transaction as the
    // mutation, so a successful request writes exactly one durable audit row
    // sharing the mutation's fate (both or neither), and a missing sink is no
    // longer possible.

    #[tokio::test]
    async fn reads_require_no_audit_sink() {
        let (state, _) = topology_state(operator_context()).await;
        let (status, _) = call(
            state.clone(),
            "GET",
            "/topology/failure-domains",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(state, "GET", "/regions", None, None).await;
        assert_eq!(status, StatusCode::OK);
    }

    #[tokio::test]
    async fn successful_mutations_write_exactly_one_durable_audit_row_each() {
        let (state, store) = topology_state_with_store(operator_context()).await;

        let (status, _) = create_failure_domain(state.clone(), "rack-1", "az-1").await;
        assert_eq!(status, StatusCode::CREATED);
        let (status, _) = call(
            state.clone(),
            "PUT",
            "/topology/failure-domains/rack-1/bindings/host/hv-1",
            None,
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(state, "PUT", "/regions/region-b", None, None).await;
        assert_eq!(status, StatusCode::OK);

        // Every successful request yielded exactly one durable audit row, all
        // carrying the operator principal + manage action (there is no sink to
        // go missing).
        let events = store.state().audit_events.clone();
        assert_eq!(events.len(), 3);
        let expected = [
            ("topology", "failure_domain", "rack-1"),
            ("topology", "binding", "rack-1"),
            ("topology", "region", "region-b"),
        ];
        for (event, (namespace, kind, id)) in events.iter().zip(expected) {
            assert_eq!(event.outcome, AuditOutcome::Succeeded);
            assert_eq!(event.principal_id.as_str(), "operator-1");
            assert_eq!(event.service_namespace.as_str(), namespace);
            assert_eq!(event.action.as_str(), "topology:ManageTopology");
            assert_eq!(
                event.resource_type.as_ref().unwrap().to_string(),
                format!("topology:{kind}")
            );
            assert_eq!(event.resource_id.as_ref().unwrap().as_str(), id);
        }
    }

    #[tokio::test]
    async fn rejected_mutation_writes_no_durable_audit_row() {
        // A request the registry/store rejects must not persist an audit row:
        // audit shares the mutation's fate (write both, or neither).
        let (state, store) = topology_state_with_store(operator_context()).await;
        let (status, _) = call(
            state,
            "POST",
            "/topology/failure-domains",
            Some(serde_json::json!({
                "id": "rack-bad",
                "class": "rack",
                "name": "x",
                "availability_domain": "ghost-az",
            })),
            None,
        )
        .await;
        // Unknown availability domain is rejected (not a 2xx), so nothing was
        // persisted — including any audit row.
        assert_ne!(status, StatusCode::CREATED);
        assert_ne!(status, StatusCode::OK);
        assert!(
            store.state().audit_events.is_empty(),
            "rejected mutation must leave no durable audit row: {status}"
        );
    }

    #[tokio::test]
    async fn list_reads_fail_closed_on_malformed_stored_rows() {
        // MEDIUM-2: a store double returning a shape-invalid failure domain
        // must be refused (503), not served as authoritative topology.
        let state = NativeApiState::new(
            None,
            pagination::CursorConfig::new(CURSOR_KEY.to_vec()).unwrap(),
            Some(Arc::new(TestIssuer(operator_context()))),
            None,
            None,
            None,
        )
        .unwrap()
        .with_topology_store(Arc::new(MalformedRowTopologyStore::default()))
        .with_authorizer(Arc::new(o3k_kernel::StaticAuthorizer::standard()));
        let (status, problem) = call(state, "GET", "/topology/failure-domains", None, None).await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(problem["code"], "NOT_AVAILABLE");
    }
}
