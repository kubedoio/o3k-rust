#[allow(clippy::wildcard_imports)]
use super::*;
use crate::plan::{
    canonical_policy_record, policy_from_canonical_record, security_group_from_policy,
    security_group_rule_from_policy, validate_policy_shape,
};

/// Canonical binding state of a port on its selected host.
///
/// The durable store persists the string projections (persistence
/// projection); this service is the only authority that transitions between
/// states. `None` in the store means no host was ever selected and no
/// observation exists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortBindingState {
    /// A create dispatch selected a host but realization is not yet observed.
    Binding,
    /// The host observed the binding as realized.
    Bound,
    /// The host observed the binding as not realized.
    Down,
    /// The host observed a terminal failure.
    Error,
}

impl PortBindingState {
    /// The durable string projection.
    pub fn as_str(self) -> &'static str {
        match self {
            PortBindingState::Binding => "binding",
            PortBindingState::Bound => "bound",
            PortBindingState::Down => "down",
            PortBindingState::Error => "error",
        }
    }

    /// Parses the durable string projection. Unknown values are rejected so
    /// free-form state can never be persisted through the service.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "binding" => Some(PortBindingState::Binding),
            "bound" => Some(PortBindingState::Bound),
            "down" => Some(PortBindingState::Down),
            "error" => Some(PortBindingState::Error),
            _ => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("unauthorized")]
    Unauthorized,
    #[error("network resource not found")]
    NotFound,
    #[error("network resource already exists or is still in use")]
    Conflict,
    #[error("network request is invalid")]
    InvalidRequest,
    #[error("quota exceeded for {key}: limit {limit}, used {used}, requested {requested}")]
    QuotaExceeded {
        key: LimitKey,
        limit: LimitValue,
        used: u64,
        requested: u64,
    },
    #[error("subnet allocation pool is exhausted")]
    PoolExhausted,
    #[error("network store error")]
    Store(#[source] o3k_store::StoreError),
    #[error("network metadata is corrupt")]
    CorruptMetadata(#[source] serde_json::Error),
}

fn map_store_error(error: o3k_store::StoreError) -> NetworkError {
    match error {
        o3k_store::StoreError::ResourceAlreadyExists => NetworkError::Conflict,
        o3k_store::StoreError::NetworkNotFound | o3k_store::StoreError::ResourceNotFound => {
            NetworkError::NotFound
        }
        o3k_store::StoreError::NetworkInUse => NetworkError::Conflict,
        o3k_store::StoreError::OwnershipConflict => NetworkError::InvalidRequest,
        o3k_store::StoreError::QuotaExceeded {
            key,
            limit,
            used,
            requested,
        } => NetworkError::QuotaExceeded {
            key,
            limit,
            used,
            requested,
        },
        o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
        other => NetworkError::Store(other),
    }
}

fn realm_delete_operation(
    project_id: &str,
    realm_id: Uuid,
) -> Result<
    (
        o3k_store::OperationRecord,
        o3k_store::CanonicalOperationRecord,
        o3k_store::IdempotencyReservationRequest,
    ),
    NetworkError,
> {
    let action =
        ActionId::new("network", "DeleteRealm").map_err(|_| NetworkError::InvalidRequest)?;
    let resource_type =
        ResourceType::new("network", "address_realm").map_err(|_| NetworkError::InvalidRequest)?;
    let operation_id = Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("o3k:network:realm-delete:{project_id}:{realm_id}").as_bytes(),
    );
    let scope = OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
    let kernel = o3k_kernel::Operation::new(
        operation_id,
        "network",
        action.clone(),
        "o3k:network-service",
        scope,
        resource_type.clone(),
        Some(ResourceId::new_unchecked(realm_id.to_string())),
        None,
    );
    let canonical = o3k_store::CanonicalOperationRecord::from_kernel_operation(&kernel)
        .map_err(map_store_error)?;
    let operation = o3k_store::OperationRecord {
        id: operation_id,
        resource_id: realm_id,
        kind: "lifecycle:realm-delete".to_owned(),
        state: o3k_store::OperationState::Pending,
        provider_operation_id: None,
        error_category: None,
        error_message: None,
    };
    let request = o3k_store::IdempotencyReservationRequest::from_semantics(
        project_id,
        action.to_string(),
        format!("canonical:realm-delete:{realm_id}"),
        &resource_type.to_string(),
        Some(&realm_id.to_string()),
        &serde_json::json!({"realm_id": realm_id}),
        operation_id,
    )
    .map_err(map_store_error)?;
    Ok((operation, canonical, request))
}

fn canonical_network_projection(network: o3k_store::CanonicalNetworkRecord) -> NetworkRecord {
    NetworkRecord {
        id: network.id,
        name: network.name,
        project_id: network.project_id,
        status: network.state.to_ascii_uppercase(),
    }
}

#[derive(Clone)]
pub struct NetworkService {
    inner: Arc<Inner>,
    lock: Arc<tokio::sync::Mutex<()>>,
    authorizer: Arc<dyn Authorizer>,
    audit_sink: Arc<dyn AuditSink>,
}

struct Inner {
    root: PathBuf,
    repository: Arc<dyn o3k_store::NetworkRepository>,
}

/// Canonical network reconstruction result.  Compatibility projections and
/// provider plans are derived from this durable graph; they are never used to
/// recover missing canonical children.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonicalNetworkSnapshot {
    pub network: o3k_store::CanonicalNetworkRecord,
    pub realms: Vec<o3k_store::CanonicalAddressRealmRecord>,
    pub pools: BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
    pub endpoints: BTreeMap<Uuid, Vec<o3k_store::CanonicalEndpointRecord>>,
    /// Canonical L3 gateway authority relevant to this network's realms.
    /// Provider plans are derived from this graph; it is not compatibility
    /// state and does not redefine AddressRealm identity.
    pub l3_gateways: Vec<(
        o3k_store::CanonicalL3GatewayRecord,
        Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>,
    )>,
}

/// Compiles the canonical gateway graph into the existing provider-neutral
/// routing intents. AddressRealm remains the unit of address interpretation;
/// this function only derives connectivity from the gateway attachments.
pub type GatewayIntentMap = BTreeMap<
    Uuid,
    (
        Vec<o3k_domain::GatewayIntent>,
        Vec<o3k_domain::EgressIntent>,
    ),
>;

pub fn compile_l3_gateway_intents(
    gateway: &o3k_store::CanonicalL3GatewayRecord,
    attachments: &[o3k_store::CanonicalL3GatewayAttachmentRecord],
    realms: &[o3k_store::CanonicalAddressRealmRecord],
    pools: &BTreeMap<Uuid, Vec<o3k_store::CanonicalAddressPoolRecord>>,
) -> Result<GatewayIntentMap, NetworkError> {
    if gateway.state != "active" || gateway.generation == 0 {
        return Err(NetworkError::InvalidRequest);
    }
    let mut realm_map = BTreeMap::new();
    for realm in realms {
        if realm.project_id != gateway.project_id || realm.state != "active" {
            continue;
        }
        let (network, prefix) = realm
            .prefix
            .split_once('/')
            .ok_or(NetworkError::InvalidRequest)?;
        let address = network.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix_len = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix = Ipv4Prefix::new(address, prefix_len).ok_or(NetworkError::InvalidRequest)?;
        realm_map.insert(realm.id, prefix);
    }
    let attached: BTreeSet<Uuid> = attachments
        .iter()
        .filter(|attachment| {
            attachment.project_id == gateway.project_id && attachment.state == "active"
        })
        .map(|attachment| attachment.realm_id)
        .collect();
    let mut result = BTreeMap::new();
    for realm_id in &attached {
        let local = realm_map.get(realm_id).ok_or(NetworkError::NotFound)?;
        let local_gateway = pools
            .get(realm_id)
            .and_then(|items| items.iter().find_map(|pool| pool.gateway))
            .or_else(|| u32::from(local.network).checked_add(1).map(Ipv4Addr::from))
            .ok_or(NetworkError::InvalidRequest)?;
        let mut routes = Vec::new();
        for remote_id in &attached {
            if remote_id != realm_id {
                routes.push(o3k_domain::GatewayIntent {
                    destination: *realm_map.get(remote_id).ok_or(NetworkError::NotFound)?,
                    gateway: local_gateway,
                    external: false,
                });
            }
        }
        let egress = gateway
            .external_realm_id
            .map(|external_realm_id| {
                vec![o3k_domain::EgressIntent {
                    external_realm_id,
                    enabled: true,
                    nat: gateway.enable_snat,
                }]
            })
            .unwrap_or_default();
        result.insert(*realm_id, (routes, egress));
    }
    Ok(result)
}

/// Result of observing one provider-owned Realm cleanup identity.  A Realm
/// remains canonical while the provider outcome is present or unknown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupObservation {
    Absent(o3k_store::CanonicalRealmBindingRecord),
    Present(o3k_store::CanonicalRealmBindingRecord),
    Unknown {
        binding: o3k_store::CanonicalRealmBindingRecord,
        reason: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RealmCleanupProgress {
    Deleting { operation_id: Uuid, generation: u64 },
    AwaitingObservation { operation_id: Uuid, generation: u64 },
    Removed { operation_id: Uuid },
}

impl NetworkService {
    /// Creates the provider-independent L3 gateway authority. This is
    /// intentionally persistence-only; Neutron projection and provider
    /// realization are layered above the canonical graph.
    pub async fn create_l3_gateway_for_project(
        &self,
        project_id: &str,
        name: String,
        external_realm_id: Option<Uuid>,
        enable_snat: bool,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        if let Some(realm_id) = external_realm_id {
            let realm = self
                .inner
                .repository
                .get_canonical_realm(project_id, &realm_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?;
            if realm.state != "active" {
                return Err(NetworkError::Conflict);
            }
        }
        let gateway = o3k_store::CanonicalL3GatewayRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            external_realm_id,
            enable_snat,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_l3_gateway(&gateway)
            .await
            .map_err(map_store_error)?;
        Ok(gateway)
    }

    pub async fn list_l3_gateways_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateways(project_id)
            .await
            .map_err(map_store_error)
    }

    /// Enumerates durable gateway deletion reservations for a fresh runtime.
    /// The returned canonical rows are recovery inputs only; provider state
    /// is never used to recreate them.
    pub async fn list_deleting_l3_gateways(
        &self,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateways_by_state("deleting")
            .await
            .map_err(map_store_error)
    }

    pub async fn list_deleting_l3_gateway_attachments(
        &self,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateway_attachments_by_state("deleting")
            .await
            .map_err(map_store_error)
    }

    /// Enumerates policy child deletion reservations for startup recovery.
    /// These rows are canonical transitional state; provider observations are
    /// used only to decide when they may be finalized.
    pub async fn list_deleting_policy_rules(
        &self,
    ) -> Result<Vec<o3k_store::CanonicalNetworkPolicyRuleRecord>, NetworkError> {
        self.inner
            .repository
            .list_deleting_policy_rules()
            .await
            .map_err(map_store_error)
    }

    pub async fn list_deleting_policy_attachments(
        &self,
    ) -> Result<Vec<o3k_store::CanonicalPolicyAttachmentRecord>, NetworkError> {
        self.inner
            .repository
            .list_deleting_policy_attachments()
            .await
            .map_err(map_store_error)
    }

    /// Resolves the network execution context for a canonical endpoint. The
    /// endpoint and realm remain canonical authority; this is only dispatch
    /// input for the host-local network execution boundary.
    pub async fn network_id_for_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<Uuid, NetworkError> {
        let endpoint = self
            .inner
            .repository
            .get_canonical_endpoint(project_id, endpoint_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.inner
            .repository
            .get_canonical_realm(project_id, &endpoint.realm_id)
            .await
            .map_err(map_store_error)?
            .map(|realm| realm.network_id)
            .ok_or(NetworkError::NotFound)
    }

    pub async fn get_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_l3_gateway(project_id, gateway_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn attach_l3_gateway_realm(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        realm_id: &Uuid,
    ) -> Result<o3k_store::CanonicalL3GatewayAttachmentRecord, NetworkError> {
        let gateway = self
            .get_l3_gateway_for_project(project_id, gateway_id)
            .await?;
        if gateway.state != "active" {
            return Err(NetworkError::Conflict);
        }
        if self
            .inner
            .repository
            .get_canonical_realm(project_id, realm_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        if self
            .inner
            .repository
            .list_canonical_l3_gateway_attachments(project_id, gateway_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|attachment| attachment.realm_id == *realm_id)
        {
            // The relation is a durable deletion reservation as well as a
            // compatibility object. Do not replace it while the provider is
            // still converging the detach.
            return Err(NetworkError::Conflict);
        }
        let attachment = o3k_store::CanonicalL3GatewayAttachmentRecord {
            id: Uuid::now_v7(),
            gateway_id: *gateway_id,
            realm_id: *realm_id,
            project_id: project_id.to_owned(),
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_l3_gateway_attachment(&attachment)
            .await
            .map_err(map_store_error)?;
        Ok(attachment)
    }

    pub async fn update_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
        name: String,
        external_realm_id: Option<Uuid>,
        enable_snat: bool,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        if let Some(realm_id) = external_realm_id {
            let realm = self
                .inner
                .repository
                .get_canonical_realm(project_id, &realm_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?;
            if realm.state != "active" {
                return Err(NetworkError::Conflict);
            }
        }
        self.inner
            .repository
            .update_canonical_l3_gateway(
                project_id,
                gateway_id,
                expected_generation,
                &name,
                external_realm_id,
                enable_snat,
            )
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_l3_gateway_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
    ) -> Result<o3k_store::CanonicalL3GatewayRecord, NetworkError> {
        self.inner
            .repository
            .begin_canonical_l3_gateway_deletion(project_id, gateway_id, expected_generation)
            .await
            .map_err(map_store_error)
    }

    /// Finalizes a gateway deletion only after the provider has withdrawn the
    /// complete gateway realization and absence has been observed.  Keeping
    /// this separate from reservation makes the deleting row restart-safe.
    pub async fn finalize_l3_gateway_deletion_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), NetworkError> {
        self.inner
            .repository
            .finalize_canonical_l3_gateway_deletion(project_id, gateway_id, expected_generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn detach_l3_gateway_realm(
        &self,
        project_id: &str,
        attachment_id: &Uuid,
        expected_generation: u64,
    ) -> Result<o3k_store::CanonicalL3GatewayAttachmentRecord, NetworkError> {
        self.inner
            .repository
            .begin_canonical_l3_gateway_attachment_deletion(
                project_id,
                attachment_id,
                expected_generation,
            )
            .await
            .map_err(map_store_error)
    }

    /// Finalizes an attachment deletion only after the complete remaining
    /// gateway snapshot has converged and the detached provider link is absent.
    pub async fn finalize_l3_gateway_realm_detachment_for_project(
        &self,
        project_id: &str,
        attachment_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), NetworkError> {
        self.inner
            .repository
            .finalize_canonical_l3_gateway_attachment_deletion(
                project_id,
                attachment_id,
                expected_generation,
            )
            .await
            .map_err(map_store_error)
    }

    pub async fn list_l3_gateway_attachments(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<Vec<o3k_store::CanonicalL3GatewayAttachmentRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_l3_gateway_attachments(project_id, gateway_id)
            .await
            .map_err(map_store_error)
    }

    /// Reconstructs the provider-independent multi-Realm gateway execution
    /// unit from canonical persistence. Linux namespace/interface details are
    /// intentionally supplied by the provider's derived Realm directory,
    /// never persisted in this plan.
    pub async fn compile_l3_gateway_execution_plan_for_project(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<o3k_domain::L3GatewayExecutionPlan, NetworkError> {
        let gateway = self
            .get_l3_gateway_for_project(project_id, gateway_id)
            .await?;
        let mut compilable_gateway = gateway.clone();
        if !matches!(gateway.state.as_str(), "active" | "deleting") {
            return Err(NetworkError::Conflict);
        }
        // The provider-neutral compiler validates an active desired snapshot.
        // A deleting row is nevertheless a valid durable removal reservation:
        // compile that snapshot without changing its persisted generation.
        compilable_gateway.state = "active".to_owned();
        let attachments = self
            .list_l3_gateway_attachments(project_id, gateway_id)
            .await?;
        let mut realms = BTreeMap::new();
        for attachment in &attachments {
            if let Some(realm) = self
                .inner
                .repository
                .get_canonical_realm(project_id, &attachment.realm_id)
                .await
                .map_err(map_store_error)?
            {
                realms.insert(realm.id, realm);
            }
        }
        if let Some(external_realm_id) = gateway.external_realm_id {
            let external = self
                .inner
                .repository
                .get_canonical_realm(project_id, &external_realm_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?;
            if external.state != "active" {
                return Err(NetworkError::Conflict);
            }
            realms.insert(external.id, external);
        }
        compile_l3_gateway_execution_plan(&compilable_gateway, &attachments, &realms)
            .map_err(|_| NetworkError::InvalidRequest)
    }

    pub async fn open(
        root: impl Into<PathBuf>,
        repository: Arc<dyn o3k_store::NetworkRepository>,
    ) -> Result<Self, NetworkError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|source| {
            NetworkError::Store(o3k_store::StoreError::CreateDataDirectory {
                path: root.clone(),
                source,
            })
        })?;
        let inner = Arc::new(Inner { root, repository });
        if inner.root.join("metadata.json").exists() {
            import_legacy_metadata(&inner.root, inner.repository.as_ref()).await?;
        }
        inner
            .repository
            .backfill_canonical_network_state()
            .await
            .map_err(map_store_error)?;
        let service = Self {
            inner,
            lock: Arc::new(tokio::sync::Mutex::new(())),
            authorizer: Arc::new(StaticAuthorizer::standard()),
            audit_sink: Arc::new(NoopAuditSink),
        };
        service.recover_realm_deletion_operations().await?;
        Ok(service)
    }

    /// Rebuilds one endpoint's effective policy from canonical reusable policy
    /// state. This is the runtime integration seam; the returned snapshot is
    /// still derived execution input and is never written back as policy
    /// authority.
    pub async fn compile_canonical_policy_for_endpoint(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
    ) -> Result<(Vec<NetworkPlanIntent>, String), CanonicalPolicyServiceError> {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .compile_endpoint(project_id, endpoint_id)
            .await
    }

    pub async fn affected_endpoints_for_canonical_policy(
        &self,
        project_id: &str,
        policy_id: Uuid,
    ) -> Result<Vec<Uuid>, CanonicalPolicyServiceError> {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .affected_endpoints_for_policy(project_id, policy_id)
            .await
    }

    pub async fn reconcile_canonical_policy_for_endpoint<P>(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
        expected_fingerprint: Option<&str>,
        provider: &P,
    ) -> Result<PolicyApplyOutcome, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .reconcile_endpoint_policy(project_id, endpoint_id, expected_fingerprint, provider)
            .await
    }

    pub async fn reconcile_canonical_policy_endpoints<P>(
        &self,
        project_id: &str,
        policy_id: Uuid,
        provider: &P,
    ) -> Result<Vec<(Uuid, PolicyApplyOutcome)>, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .reconcile_policy_endpoints(project_id, policy_id, provider)
            .await
    }

    pub async fn recover_canonical_policy_realizations<P>(
        &self,
        project_id: &str,
        provider: &P,
    ) -> Result<Vec<(Uuid, PolicyApplyOutcome)>, CanonicalPolicyServiceError>
    where
        P: PolicySnapshotRealizer,
    {
        CanonicalPolicyService::new(self.inner.repository.clone())
            .recover_policy_realizations(project_id, provider)
            .await
    }

    #[must_use]
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self
    }

    #[must_use]
    pub fn with_audit_sink(mut self, audit_sink: Arc<dyn AuditSink>) -> Self {
        self.audit_sink = audit_sink;
        self
    }

    async fn authorize_canonical_action(
        &self,
        auth: &AuthContext,
        action_name: &str,
        resource_name: &str,
        resource_id: Option<Uuid>,
        parent: Option<(&str, Uuid)>,
    ) -> Result<(ServiceNamespace, ActionId, ResourceType), NetworkError> {
        let namespace = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let action =
            ActionId::new("network", action_name).map_err(|_| NetworkError::InvalidRequest)?;
        let resource_type = ResourceType::new("network", resource_name)
            .map_err(|_| NetworkError::InvalidRequest)?;
        let owner_scope = match resource_id {
            Some(id) => self
                .inner
                .repository
                .get_canonical_owner(resource_name, &id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::NotFound)?,
            None => match parent {
                Some((parent_name, parent_id)) => self
                    .inner
                    .repository
                    .get_canonical_owner(parent_name, &parent_id)
                    .await
                    .map_err(map_store_error)?
                    .ok_or(NetworkError::NotFound)?,
                None => auth.effective_scope().id().as_str().to_owned(),
            },
        };
        let owner_scope = ScopeId::new(owner_scope).map_err(|_| NetworkError::InvalidRequest)?;
        let target = match resource_id {
            Some(id) => ResourceTarget::instance(
                resource_type.clone(),
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(owner_scope.clone()),
            ),
            None => ResourceTarget::collection(resource_type.clone(), Some(owner_scope.clone())),
        };
        let decision = self.authorizer.authorize(&AuthorizationRequest {
            auth_context: auth,
            action: action.clone(),
            resource_target: target,
        });
        if !decision.is_allowed() {
            self.audit_sink.record(
                &AuditEvent::from_auth(
                    auth,
                    namespace.clone(),
                    action.clone(),
                    AuditOutcome::Denied,
                )
                .with_resource(
                    resource_type.clone(),
                    resource_id.and_then(|id| ResourceId::new(id.to_string()).ok()),
                    Some(OwnershipScope::project(owner_scope, None, None)),
                )
                .with_decision(decision.clone())
                .with_reason("unauthorized"),
            );
            return Err(match decision.reason() {
                DecisionReason::ScopeMismatch | DecisionReason::MissingOwnership => {
                    NetworkError::NotFound
                }
                _ => NetworkError::Unauthorized,
            });
        }
        Ok((namespace, action, resource_type))
    }

    fn audit_canonical_result(
        &self,
        auth: &AuthContext,
        namespace: ServiceNamespace,
        action: ActionId,
        resource_type: ResourceType,
        resource_id: Option<Uuid>,
        result: Result<(), &NetworkError>,
    ) {
        let outcome = if result.is_ok() {
            AuditOutcome::Succeeded
        } else {
            AuditOutcome::Failed
        };
        let mut event = AuditEvent::from_auth(auth, namespace, action, outcome).with_resource(
            resource_type,
            resource_id.and_then(|id| ResourceId::new(id.to_string()).ok()),
            Some(auth.effective_scope().clone()),
        );
        if let Err(error) = result {
            event = event.with_reason(error.to_string());
        }
        self.audit_sink.record(&event);
    }

    async fn recover_realm_deletion_operations(&self) -> Result<(), NetworkError> {
        let operations = self
            .inner
            .repository
            .list_non_terminal_lifecycle_operations()
            .await
            .map_err(map_store_error)?;
        for operation in operations {
            if operation.kind != "lifecycle:realm-delete" {
                continue;
            }
            let canonical = self
                .inner
                .repository
                .get_canonical_operation(operation.id)
                .await
                .map_err(map_store_error)?;
            let realm_id = canonical
                .resource_id
                .as_deref()
                .ok_or(NetworkError::InvalidRequest)?
                .parse::<Uuid>()
                .map_err(|_| NetworkError::InvalidRequest)?;
            let realm = self
                .inner
                .repository
                .get_canonical_realm(&canonical.owner_scope, &realm_id)
                .await
                .map_err(map_store_error)?;
            let Some(realm) = realm else {
                let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Succeeded,
                    canonical.attempt.saturating_add(1),
                    canonical.started_at.clone(),
                    Some(
                        canonical
                            .started_at
                            .clone()
                            .unwrap_or_else(|| canonical.created_at.clone()),
                    ),
                    None,
                )
                .map_err(map_store_error)?;
                self.inner
                    .repository
                    .update_canonical_operation_lifecycle(operation.id, &update)
                    .await
                    .map_err(map_store_error)?;
                continue;
            };
            if realm.state == "active" {
                let deleting = match self
                    .inner
                    .repository
                    .begin_canonical_realm_deletion(
                        &canonical.owner_scope,
                        &realm_id,
                        realm.generation,
                    )
                    .await
                {
                    Ok(value) => value,
                    Err(o3k_store::StoreError::NetworkInUse) => {
                        let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                            o3k_kernel::OperationState::Retryable,
                            canonical.attempt.saturating_add(1),
                            canonical.started_at.clone(),
                            None,
                            Some("realm still has canonical dependents".to_owned()),
                        )
                        .map_err(map_store_error)?;
                        self.inner
                            .repository
                            .update_canonical_operation_lifecycle(operation.id, &update)
                            .await
                            .map_err(map_store_error)?;
                        continue;
                    }
                    Err(error) => return Err(map_store_error(error)),
                };
                self.inner
                    .repository
                    .update_resource(
                        realm_id,
                        i64::try_from(realm.generation)
                            .map_err(|_| NetworkError::InvalidRequest)?,
                        "deleting",
                        "deleting",
                        i64::try_from(deleting.generation)
                            .map_err(|_| NetworkError::InvalidRequest)?,
                        None,
                    )
                    .await
                    .map_err(map_store_error)?;
                let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                    o3k_kernel::OperationState::Running,
                    canonical.attempt.saturating_add(1),
                    Some(
                        canonical
                            .started_at
                            .clone()
                            .unwrap_or_else(|| canonical.created_at.clone()),
                    ),
                    None,
                    None,
                )
                .map_err(map_store_error)?;
                self.inner
                    .repository
                    .update_canonical_operation_lifecycle(operation.id, &update)
                    .await
                    .map_err(map_store_error)?;
                debug_assert_eq!(deleting.state, "deleting");
            }
        }
        Ok(())
    }

    /// Creates only the canonical Network row.  Address realms, pools and
    /// endpoints are independent child resources and are intentionally not
    /// synthesized here.
    pub async fn create_canonical_network_for_project(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        if project_id.trim().is_empty() || name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
        if self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|network| network.name == name)
        {
            return Err(NetworkError::Conflict);
        }
        let network = o3k_store::CanonicalNetworkRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            admin_state_up: true,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_network(&network)
            .await
            .map_err(map_store_error)?;
        Ok(network)
    }

    /// Authenticated entry point for canonical Network creation. The
    /// project-scoped primitive remains intentionally separate for internal
    /// reconciliation and migration callers.
    pub async fn create_canonical_network(
        &self,
        auth: &AuthContext,
        name: String,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "CreateNetwork", "network", None, None)
            .await?;
        let result = self
            .create_canonical_network_for_project(auth.effective_scope().id().as_str(), name)
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn get_canonical_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn get_canonical_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ReadNetwork", "network", Some(id), None)
            .await?;
        let result = self
            .get_canonical_network_for_project(auth.effective_scope().id().as_str(), id)
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn list_canonical_networks_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::CanonicalNetworkRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_canonical_networks(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<o3k_store::CanonicalNetworkRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ListNetworks", "network", None, None)
            .await?;
        let result = self
            .list_canonical_networks_for_project(auth.effective_scope().id().as_str())
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock.lock().await;
        self.inner
            .repository
            .delete_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_canonical_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "DeleteNetwork", "network", Some(id), None)
            .await?;
        let result = self
            .delete_canonical_network_for_project(auth.effective_scope().id().as_str(), id)
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn create_canonical_realm_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        prefix: String,
        overlapping_prefixes: bool,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let prefix = Ipv4Net::parse(&prefix)?.canonical();
        let _guard = self.lock.lock().await;
        let network = self
            .inner
            .repository
            .get_canonical_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if network.state != "active" {
            return Err(NetworkError::Conflict);
        }
        let realm = o3k_store::CanonicalAddressRealmRecord {
            id: Uuid::now_v7(),
            network_id,
            project_id: project_id.to_owned(),
            prefix,
            overlapping_prefixes,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_realm(&realm)
            .await
            .map_err(map_store_error)?;
        if let Err(error) = self
            .inner
            .repository
            .insert_resource(&o3k_store::ResourceRecord {
                id: realm.id,
                kind: "network:address_realm".to_owned(),
                project_id: realm.project_id.clone(),
                generation: 1,
                observed_generation: 1,
                desired_state: realm.state.clone(),
                observed_state: realm.state.clone(),
                provider_id: None,
            })
            .await
        {
            let _ = self
                .inner
                .repository
                .delete_canonical_realm(project_id, &realm.id)
                .await;
            return Err(map_store_error(error));
        }
        Ok(realm)
    }

    pub async fn create_canonical_realm(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        prefix: String,
        overlapping_prefixes: bool,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateAddressRealm",
                "address_realm",
                None,
                Some(("network", network_id)),
            )
            .await?;
        let result = self
            .create_canonical_realm_for_project(
                auth.effective_scope().id().as_str(),
                network_id,
                prefix,
                overlapping_prefixes,
            )
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn list_canonical_realms_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressRealmRecord>, NetworkError> {
        if self
            .get_canonical_network_for_project(project_id, network_id)
            .await
            .is_err()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_canonical_realms(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressRealmRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListAddressRealms",
                "address_realm",
                None,
                Some(("network", network_id)),
            )
            .await?;
        let result = self
            .list_canonical_realms_for_project(auth.effective_scope().id().as_str(), network_id)
            .await;
        let audit_result = result.as_ref().map(|_| ());
        self.audit_canonical_result(auth, namespace, action, resource_type, None, audit_result);
        result
    }

    pub async fn get_canonical_realm(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ReadAddressRealm",
                "address_realm",
                Some(realm_id),
                None,
            )
            .await?;
        let result = self
            .inner
            .repository
            .get_canonical_realm(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(realm_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Owner-scoped lookup used by compatibility projections after the
    /// request has already been authorized at the API boundary.
    pub async fn get_canonical_realm_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<o3k_store::CanonicalAddressRealmRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn delete_canonical_realm_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<(), NetworkError> {
        let progress = self
            .begin_canonical_realm_deletion_for_project(project_id, realm_id)
            .await?;
        let operation_id = match progress {
            RealmCleanupProgress::Deleting { operation_id, .. }
            | RealmCleanupProgress::AwaitingObservation { operation_id, .. }
            | RealmCleanupProgress::Removed { operation_id } => operation_id,
        };
        let bindings = self
            .inner
            .repository
            .list_canonical_realm_bindings(&realm_id)
            .await
            .map_err(map_store_error)?;
        if bindings.is_empty() {
            match self
                .observe_canonical_realm_cleanup_for_project(project_id, realm_id, Vec::new())
                .await?
            {
                RealmCleanupProgress::Removed { .. } => Ok(()),
                _ => Err(NetworkError::Conflict),
            }
        } else {
            // No provider observation is available at this service boundary;
            // retaining Deleting is the safe result. A provider/reconciler
            // must call the explicit observation method before final removal.
            let _ = operation_id;
            Err(NetworkError::Conflict)
        }
    }

    pub async fn delete_canonical_realm(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "DeleteAddressRealm",
                "address_realm",
                Some(realm_id),
                None,
            )
            .await?;
        let result = self
            .delete_canonical_realm_for_project(auth.effective_scope().id().as_str(), realm_id)
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(realm_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Durably accepts a Realm deletion before any provider mutation. The
    /// operation identity is deterministic, so retries join the same
    /// workflow even after a process restart.
    pub async fn begin_canonical_realm_deletion_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
    ) -> Result<RealmCleanupProgress, NetworkError> {
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let (operation, canonical, request) = realm_delete_operation(project_id, realm_id)?;
        let accepted = self
            .inner
            .repository
            .create_or_replay_canonical_scoped_operation(&operation, &canonical, &request)
            .await
            .map_err(map_store_error)?;
        let operation_id = match accepted {
            o3k_store::IdempotencyReservation::Conflict => return Err(NetworkError::Conflict),
            o3k_store::IdempotencyReservation::Created(id)
            | o3k_store::IdempotencyReservation::ExistingEquivalent(id) => id,
        };
        if realm.state == "deleting" {
            return Ok(RealmCleanupProgress::AwaitingObservation {
                operation_id,
                generation: realm.generation,
            });
        }
        if realm.state != "active" {
            let _ = self
                .inner
                .repository
                .update_operation(
                    operation_id,
                    o3k_store::OperationState::Failed,
                    None,
                    Some("invalid_state"),
                    Some("Realm is not deletable in its current lifecycle state"),
                )
                .await;
            return Err(NetworkError::Conflict);
        }
        let deleting = match self
            .inner
            .repository
            .begin_canonical_realm_deletion(project_id, &realm_id, realm.generation)
            .await
        {
            Ok(value) => value,
            Err(error) => {
                let state = if matches!(error, o3k_store::StoreError::NetworkInUse) {
                    o3k_store::OperationState::Retryable
                } else {
                    o3k_store::OperationState::Failed
                };
                let _ = self
                    .inner
                    .repository
                    .update_operation(
                        operation_id,
                        state,
                        None,
                        Some("rejected"),
                        Some(&error.to_string()),
                    )
                    .await;
                return Err(map_store_error(error));
            }
        };
        self.inner
            .repository
            .update_resource(
                realm_id,
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                "deleting",
                "deleting",
                i64::try_from(deleting.generation).map_err(|_| NetworkError::InvalidRequest)?,
                None,
            )
            .await
            .map_err(map_store_error)?;
        let lifecycle = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::Running,
            1,
            Some(canonical.created_at.clone()),
            None,
            None,
        )
        .map_err(map_store_error)?;
        self.inner
            .repository
            .update_canonical_operation_lifecycle(operation_id, &lifecycle)
            .await
            .map_err(map_store_error)?;
        Ok(RealmCleanupProgress::Deleting {
            operation_id,
            generation: deleting.generation,
        })
    }

    /// Applies a provider observation to a durable Realm deletion. Every
    /// current binding must be identified exactly; absent observations remove
    /// only the matching owned binding after generation validation.
    pub async fn observe_canonical_realm_cleanup_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        observations: Vec<RealmCleanupObservation>,
    ) -> Result<RealmCleanupProgress, NetworkError> {
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if realm.state != "deleting" {
            return Err(NetworkError::Conflict);
        }
        let (operation, _, _) = realm_delete_operation(project_id, realm_id)?;
        let operation_record = self
            .inner
            .repository
            .get_operation(operation.id)
            .await
            .map_err(map_store_error)?;
        let canonical = self
            .inner
            .repository
            .get_canonical_operation(operation.id)
            .await
            .map_err(map_store_error)?;
        if operation_record.resource_id != realm_id
            || canonical.owner_scope != project_id
            || canonical.resource_id.as_deref() != Some(&realm_id.to_string())
        {
            return Err(NetworkError::InvalidRequest);
        }
        let bindings = self
            .inner
            .repository
            .list_canonical_realm_bindings(&realm_id)
            .await
            .map_err(map_store_error)?;
        if observations.len() != bindings.len() {
            return Err(NetworkError::Conflict);
        }
        let same_binding =
            |left: &o3k_store::CanonicalRealmBindingRecord,
             right: &o3k_store::CanonicalRealmBindingRecord| {
                left == right && left.realm_id == realm_id
            };
        for binding in &bindings {
            let matching: Vec<_> = observations
                .iter()
                .filter(|observation| match observation {
                    RealmCleanupObservation::Absent(value)
                    | RealmCleanupObservation::Present(value) => same_binding(binding, value),
                    RealmCleanupObservation::Unknown { binding: value, .. } => {
                        same_binding(binding, value)
                    }
                })
                .collect();
            if matching.len() != 1 {
                return Err(NetworkError::Conflict);
            }
            match matching[0] {
                RealmCleanupObservation::Unknown { reason, .. } => {
                    let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::UnknownOutcome,
                        canonical.attempt.saturating_add(1),
                        canonical.started_at.clone(),
                        None,
                        Some(reason.clone()),
                    )
                    .map_err(map_store_error)?;
                    self.inner
                        .repository
                        .update_canonical_operation_lifecycle(operation.id, &update)
                        .await
                        .map_err(map_store_error)?;
                    return Ok(RealmCleanupProgress::AwaitingObservation {
                        operation_id: operation.id,
                        generation: realm.generation,
                    });
                }
                RealmCleanupObservation::Present(_) => {
                    let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
                        o3k_kernel::OperationState::Retryable,
                        canonical.attempt.saturating_add(1),
                        canonical.started_at.clone(),
                        None,
                        Some("owned provider Realm state is still present".to_owned()),
                    )
                    .map_err(map_store_error)?;
                    self.inner
                        .repository
                        .update_canonical_operation_lifecycle(operation.id, &update)
                        .await
                        .map_err(map_store_error)?;
                    return Ok(RealmCleanupProgress::AwaitingObservation {
                        operation_id: operation.id,
                        generation: realm.generation,
                    });
                }
                RealmCleanupObservation::Absent(_) => {}
            }
        }
        for binding in bindings {
            self.inner
                .repository
                .delete_canonical_realm_binding(&binding, realm.generation)
                .await
                .map_err(map_store_error)?;
        }
        self.inner
            .repository
            .finalize_canonical_realm_deletion(project_id, &realm_id, realm.generation)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .update_resource(
                realm_id,
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                "deleted",
                "deleted",
                i64::try_from(realm.generation).map_err(|_| NetworkError::InvalidRequest)?,
                None,
            )
            .await;
        let update = o3k_store::CanonicalOperationLifecycleUpdate::new(
            o3k_kernel::OperationState::Succeeded,
            canonical.attempt.saturating_add(1),
            canonical.started_at.clone(),
            Some(
                canonical
                    .started_at
                    .clone()
                    .unwrap_or_else(|| canonical.created_at.clone()),
            ),
            None,
        )
        .map_err(map_store_error)?;
        self.inner
            .repository
            .update_canonical_operation_lifecycle(operation.id, &update)
            .await
            .map_err(map_store_error)?;
        Ok(RealmCleanupProgress::Removed {
            operation_id: operation.id,
        })
    }

    pub async fn create_canonical_pool_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        prefix: String,
        gateway: Option<Ipv4Addr>,
        first_usable: Ipv4Addr,
        last_usable: Ipv4Addr,
    ) -> Result<o3k_store::CanonicalAddressPoolRecord, NetworkError> {
        let pool_prefix_net = Ipv4Net::parse(&prefix)?;
        let pool_prefix = pool_prefix_net.canonical();
        if first_usable > last_usable
            || !pool_prefix_net.contains(first_usable)
            || !pool_prefix_net.contains(last_usable)
        {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if realm.state != "active" {
            return Err(NetworkError::Conflict);
        }
        let realm_prefix = Ipv4Net::parse(&realm.prefix)?;
        if !realm_prefix.contains(pool_prefix_net.network)
            || pool_prefix_net.prefix < realm_prefix.prefix
        {
            return Err(NetworkError::InvalidRequest);
        }
        if gateway.is_some_and(|value| !pool_prefix_net.contains(value)) {
            return Err(NetworkError::InvalidRequest);
        }
        let pool = o3k_store::CanonicalAddressPoolRecord {
            id: Uuid::now_v7(),
            realm_id,
            project_id: project_id.to_owned(),
            prefix: pool_prefix,
            gateway,
            first_usable,
            last_usable,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_pool(&pool)
            .await
            .map_err(map_store_error)?;
        Ok(pool)
    }

    pub async fn create_canonical_pool(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
        prefix: String,
        gateway: Option<Ipv4Addr>,
        first_usable: Ipv4Addr,
        last_usable: Ipv4Addr,
    ) -> Result<o3k_store::CanonicalAddressPoolRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateAddressPool",
                "address_pool",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .create_canonical_pool_for_project(
                auth.effective_scope().id().as_str(),
                realm_id,
                prefix,
                gateway,
                first_usable,
                last_usable,
            )
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn list_canonical_pools(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalAddressPoolRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListAddressPools",
                "address_pool",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .inner
            .repository
            .list_canonical_pools(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_pool(
        &self,
        auth: &AuthContext,
        pool_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "DeleteAddressPool",
                "address_pool",
                Some(pool_id),
                None,
            )
            .await?;
        let result = self
            .inner
            .repository
            .delete_canonical_pool(auth.effective_scope().id().as_str(), &pool_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(pool_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn create_canonical_endpoint_for_project(
        &self,
        project_id: &str,
        realm_id: Uuid,
        fixed_ip: Ipv4Addr,
        mac: String,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        if mac.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock.lock().await;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if !Ipv4Net::parse(&realm.prefix)?.contains(fixed_ip) || realm.state != "active" {
            return Err(NetworkError::InvalidRequest);
        }
        let endpoint = o3k_store::CanonicalEndpointRecord {
            id: Uuid::now_v7(),
            realm_id,
            project_id: project_id.to_owned(),
            fixed_ip,
            mac,
            generation: 1,
            state: "active".to_owned(),
        };
        self.inner
            .repository
            .insert_canonical_endpoint(&endpoint)
            .await
            .map_err(map_store_error)?;
        Ok(endpoint)
    }

    pub async fn create_canonical_endpoint(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
        fixed_ip: Ipv4Addr,
        mac: String,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "CreateEndpoint",
                "endpoint",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .create_canonical_endpoint_for_project(
                auth.effective_scope().id().as_str(),
                realm_id,
                fixed_ip,
                mac,
            )
            .await;
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn list_canonical_endpoints(
        &self,
        auth: &AuthContext,
        realm_id: Uuid,
    ) -> Result<Vec<o3k_store::CanonicalEndpointRecord>, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(
                auth,
                "ListEndpoints",
                "endpoint",
                None,
                Some(("address_realm", realm_id)),
            )
            .await?;
        let result = self
            .inner
            .repository
            .list_canonical_endpoints(auth.effective_scope().id().as_str(), &realm_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            None,
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn get_canonical_endpoint(
        &self,
        auth: &AuthContext,
        endpoint_id: Uuid,
    ) -> Result<o3k_store::CanonicalEndpointRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "ReadEndpoint", "endpoint", Some(endpoint_id), None)
            .await?;
        let result = self
            .inner
            .repository
            .get_canonical_endpoint(auth.effective_scope().id().as_str(), &endpoint_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(endpoint_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_canonical_endpoint(
        &self,
        auth: &AuthContext,
        endpoint_id: Uuid,
    ) -> Result<(), NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "DeleteEndpoint", "endpoint", Some(endpoint_id), None)
            .await?;
        let result = self
            .inner
            .repository
            .delete_canonical_endpoint(auth.effective_scope().id().as_str(), &endpoint_id)
            .await
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(endpoint_id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    /// Reconstructs canonical execution inputs from durable rows. Empty child
    /// collections are valid and never become NotFound or placeholder realms.
    pub async fn reconstruct_canonical_network(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<CanonicalNetworkSnapshot, NetworkError> {
        let network = self
            .get_canonical_network_for_project(project_id, network_id)
            .await?;
        let realms = self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?;
        let mut pools = BTreeMap::new();
        let mut endpoints = BTreeMap::new();
        for realm in &realms {
            if realm.network_id != network.id || realm.project_id != network.project_id {
                return Err(NetworkError::InvalidRequest);
            }
            pools.insert(
                realm.id,
                self.inner
                    .repository
                    .list_canonical_pools(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?,
            );
            endpoints.insert(
                realm.id,
                self.inner
                    .repository
                    .list_canonical_endpoints(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?,
            );
        }
        let realm_ids: BTreeSet<Uuid> = realms.iter().map(|realm| realm.id).collect();
        let mut l3_gateways = Vec::new();
        for gateway in self
            .inner
            .repository
            .list_canonical_l3_gateways(project_id)
            .await
            .map_err(map_store_error)?
        {
            let attachments = self
                .inner
                .repository
                .list_canonical_l3_gateway_attachments(project_id, &gateway.id)
                .await
                .map_err(map_store_error)?;
            let relevant: Vec<_> = attachments
                .into_iter()
                .filter(|attachment| realm_ids.contains(&attachment.realm_id))
                .collect();
            if !relevant.is_empty() {
                l3_gateways.push((gateway, relevant));
            }
        }
        Ok(CanonicalNetworkSnapshot {
            network,
            realms,
            pools,
            endpoints,
            l3_gateways,
        })
    }

    pub async fn create_network(
        &self,
        auth: &AuthContext,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreateNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreateNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        match self
            .create_network_for_project(auth.effective_scope().id().as_str(), name)
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "network").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "network".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_network_for_project(
        &self,
        project_id: &str,
        name: String,
    ) -> Result<NetworkRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|network| network.name == name)
        {
            return Err(NetworkError::Conflict);
        }
        let network = NetworkRecord {
            id: Uuid::now_v7(),
            name,
            project_id: project_id.to_owned(),
            status: "ACTIVE".to_owned(),
        };
        let canonical = o3k_store::CanonicalNetworkRecord {
            id: network.id,
            project_id: network.project_id.clone(),
            name: network.name.clone(),
            admin_state_up: true,
            generation: 1,
            state: "active".to_owned(),
        };
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(LimitKey::network_networks(), 1)];
        let op_id = format!("o3k:network:create:{}:{}", project_id, network.id);
        let quota_res = self
            .inner
            .repository
            .reserve_quota(&scope, &op_id, &amounts)
            .await
            .map_err(|err| match err {
                o3k_store::StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => NetworkError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
                other => map_store_error(other),
            })?;

        match self
            .inner
            .repository
            .insert_canonical_network(&canonical)
            .await
        {
            Ok(()) => {
                if let Err(error) = self.inner.repository.insert_network(&network).await {
                    let _ = self
                        .inner
                        .repository
                        .delete_canonical_network(project_id, &network.id)
                        .await;
                    let _ = self
                        .inner
                        .repository
                        .release_reservation(&quota_res.id)
                        .await;
                    return Err(map_store_error(error));
                }
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                Ok(network)
            }
            Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(NetworkError::Conflict)
            }
            Err(error) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(map_store_error(error))
            }
        }
    }

    pub async fn list_security_groups_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<o3k_store::SecurityGroupRecord>, NetworkError> {
        self.inner
            .repository
            .list_reusable_policies(project_id)
            .await
            .map(|policies| {
                policies
                    .into_iter()
                    .map(security_group_from_policy)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        self.inner
            .repository
            .get_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)?
            .map(security_group_from_policy)
            .ok_or(NetworkError::NotFound)
    }

    pub async fn create_security_group_for_project(
        &self,
        project_id: &str,
        name: String,
        description: String,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        if project_id.trim().is_empty() || name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let group = o3k_store::SecurityGroupRecord {
            id: Uuid::now_v7(),
            project_id: project_id.to_owned(),
            name,
            description,
        };
        self.inner
            .repository
            .insert_reusable_policy(&o3k_store::CanonicalReusableNetworkPolicyRecord {
                id: group.id,
                project_id: group.project_id.clone(),
                name: group.name.clone(),
                description: group.description.clone(),
                stateful_mode: "Stateful".to_owned(),
                unmatched_action: "Deny".to_owned(),
                generation: 1,
                state: "active".to_owned(),
                created_at: "2026-08-26T00:00:00Z".to_owned(),
                updated_at: "2026-08-26T00:00:00Z".to_owned(),
            })
            .await
            .map_err(map_store_error)?;
        Ok(group)
    }

    pub async fn update_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: String,
        description: String,
    ) -> Result<o3k_store::SecurityGroupRecord, NetworkError> {
        if name.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let current = self
            .inner
            .repository
            .get_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let updated = self
            .inner
            .repository
            .update_reusable_policy(
                &o3k_store::CanonicalReusableNetworkPolicyRecord {
                    name,
                    description,
                    updated_at: "2026-08-26T00:00:00Z".to_owned(),
                    generation: current.generation.saturating_add(1),
                    ..current
                },
                current.generation,
            )
            .await
            .map_err(map_store_error)?;
        Ok(security_group_from_policy(updated))
    }

    pub async fn delete_security_group_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_reusable_policy(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_security_group_rules_for_project(
        &self,
        project_id: &str,
        group_id: Uuid,
    ) -> Result<Vec<o3k_store::SecurityGroupRuleRecord>, NetworkError> {
        if self
            .inner
            .repository
            .get_reusable_policy(project_id, &group_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .list_policy_rules(project_id, &group_id)
            .await
            .map(|rules| {
                rules
                    .into_iter()
                    .map(security_group_rule_from_policy)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_security_group_rule_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::SecurityGroupRuleRecord, NetworkError> {
        self.inner
            .repository
            .get_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)?
            .map(security_group_rule_from_policy)
            .ok_or(NetworkError::NotFound)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_security_group_rule_for_project(
        &self,
        project_id: &str,
        group_id: Uuid,
        direction: String,
        protocol: String,
        port_min: Option<u16>,
        port_max: Option<u16>,
        remote_ip_prefix: Option<String>,
    ) -> Result<o3k_store::SecurityGroupRuleRecord, NetworkError> {
        let direction = parse_security_group_direction(&direction)?;
        let protocol_value = parse_security_group_protocol(&protocol)?;
        if matches!(protocol_value, NetworkProtocol::Icmp | NetworkProtocol::Any)
            && (port_min.is_some() || port_max.is_some())
        {
            return Err(NetworkError::InvalidRequest);
        }
        match (port_min, port_max) {
            (Some(start), Some(end)) if start <= end => {}
            (None, None) => {}
            _ => return Err(NetworkError::InvalidRequest),
        }
        if let Some(prefix) = remote_ip_prefix.as_deref() {
            parse_security_group_prefix(prefix)?;
        }
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_reusable_policy(project_id, &group_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let rule = o3k_store::CanonicalNetworkPolicyRuleRecord {
            id: Uuid::now_v7(),
            policy_id: group_id,
            project_id: project_id.to_owned(),
            direction: match direction {
                PolicyDirection::Ingress => "Ingress",
                PolicyDirection::Egress => "Egress",
            }
            .to_owned(),
            protocol: match protocol_value {
                NetworkProtocol::Any => "Any",
                NetworkProtocol::Tcp => "Tcp",
                NetworkProtocol::Udp => "Udp",
                NetworkProtocol::Icmp => "Icmp",
            }
            .to_owned(),
            address_family: "Ipv4".to_owned(),
            port_min,
            port_max,
            remote_selector: remote_ip_prefix,
            action: "Allow".to_owned(),
            state: "active".to_owned(),
            generation: 1,
            enforcement_key: String::new(),
        };
        let remote = rule
            .remote_selector
            .clone()
            .unwrap_or_else(|| "-".to_owned());
        let ports = rule
            .port_min
            .zip(rule.port_max)
            .map_or_else(|| "-".to_owned(), |(min, max)| format!("{min}-{max}"));
        let mut rule = rule;
        rule.enforcement_key = format!(
            "{}|{}|{}|{}|{}|{}",
            rule.direction, rule.address_family, rule.protocol, ports, remote, rule.action
        );
        self.inner
            .repository
            .insert_policy_rule(&rule)
            .await
            .map_err(map_store_error)?;
        Ok(security_group_rule_from_policy(rule))
    }

    pub async fn delete_security_group_rule_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)
    }

    pub async fn begin_security_group_rule_deletion_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<o3k_store::CanonicalNetworkPolicyRuleRecord, NetworkError> {
        let _guard = self.lock().await;
        let rule = self
            .inner
            .repository
            .get_policy_rule(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.inner
            .repository
            .begin_policy_rule_deletion(project_id, &id, rule.generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn finalize_security_group_rule_deletion_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        deleting_generation: u64,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .finalize_policy_rule_deletion(project_id, &id, deleting_generation)
            .await
            .map_err(map_store_error)
    }

    pub async fn list_security_group_bindings_for_project(
        &self,
        project_id: &str,
        endpoint_id: Option<Uuid>,
    ) -> Result<Vec<o3k_store::SecurityGroupBindingRecord>, NetworkError> {
        let attachments = if let Some(endpoint_id) = endpoint_id {
            self.inner
                .repository
                .list_endpoint_policy_attachments(project_id, &endpoint_id)
                .await
                .map_err(map_store_error)?
        } else {
            let policies = self
                .inner
                .repository
                .list_reusable_policies(project_id)
                .await
                .map_err(map_store_error)?;
            let mut all = Vec::new();
            for policy in policies {
                all.extend(
                    self.inner
                        .repository
                        .list_policy_attachments(project_id, &policy.id)
                        .await
                        .map_err(map_store_error)?,
                );
            }
            all
        };
        Ok(attachments
            .into_iter()
            .filter(|attachment| attachment.state == "active")
            .map(|attachment| o3k_store::SecurityGroupBindingRecord {
                project_id: attachment.project_id,
                endpoint_id: attachment.endpoint_id,
                security_group_id: attachment.policy_id,
            })
            .collect())
    }

    pub async fn replace_security_group_bindings_for_project(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
        group_ids: Vec<Uuid>,
    ) -> Result<Vec<o3k_store::CanonicalPolicyAttachmentRecord>, NetworkError> {
        let _guard = self.lock().await;
        if self
            .inner
            .repository
            .get_port(project_id, &endpoint_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .replace_policy_attachment_set(project_id, &endpoint_id, &group_ids)
            .await
            .map_err(map_store_error)
    }

    pub async fn finalize_policy_attachment_deletion_for_project(
        &self,
        project_id: &str,
        attachment_id: Uuid,
        deleting_generation: u64,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .finalize_policy_attachment_deletion(project_id, &attachment_id, deleting_generation)
            .await
            .map_err(map_store_error)
    }

    /// Returns the durable canonical policy rules for a network. A network
    /// without policy state is intentionally an empty policy, not an implicit
    /// provider default.
    pub async fn list_policies_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<Vec<PolicyIntent>, NetworkError> {
        if self
            .inner
            .repository
            .get_canonical_network(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .is_none()
        {
            return Err(NetworkError::NotFound);
        }
        let mut policies = self
            .inner
            .repository
            .list_canonical_policies(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .map(policy_from_canonical_record)
            .collect::<Result<Vec<_>, _>>()?;
        for port in self
            .list_ports_for_project(project_id)
            .await?
            .into_iter()
            .filter(|port| port.network_id == network_id)
        {
            let bindings = self
                .inner
                .repository
                .list_endpoint_policy_attachments(project_id, &port.id)
                .await
                .map_err(map_store_error)?;
            for binding in bindings
                .into_iter()
                .filter(|binding| binding.state == "active")
            {
                let Some(group) = self
                    .inner
                    .repository
                    .get_reusable_policy(project_id, &binding.policy_id)
                    .await
                    .map_err(map_store_error)?
                else {
                    return Err(NetworkError::InvalidRequest);
                };
                for rule in self
                    .inner
                    .repository
                    .list_policy_rules(project_id, &group.id)
                    .await
                    .map_err(map_store_error)?
                    .into_iter()
                    .filter(|rule| rule.state == "active")
                {
                    let direction = parse_security_group_direction(&rule.direction)?;
                    let remote = rule
                        .remote_selector
                        .as_deref()
                        .map(parse_security_group_prefix)
                        .transpose()?;
                    let ports = match (rule.port_min, rule.port_max) {
                        (Some(start), Some(end)) => Some(PortRange { start, end }),
                        (None, None) => None,
                        _ => return Err(NetworkError::InvalidRequest),
                    };
                    policies.push(PolicyIntent {
                        id: rule.id,
                        endpoint_id: port.id,
                        direction,
                        protocol: parse_security_group_protocol(&rule.protocol)?,
                        ports,
                        source: (direction == PolicyDirection::Ingress)
                            .then_some(remote)
                            .flatten(),
                        destination: (direction == PolicyDirection::Egress)
                            .then_some(remote)
                            .flatten(),
                        action: PolicyAction::Allow,
                    });
                }
            }
        }
        policies.sort_by_key(|policy| policy.id);
        Ok(policies)
    }

    /// Resolves canonical unmatched-action defaults for the active policies
    /// attached to one endpoint. Defaults are derived execution input; the
    /// reusable policy repository remains the sole desired-state authority.
    pub async fn policy_defaults_for_endpoint(
        &self,
        project_id: &str,
        endpoint_id: Uuid,
    ) -> Result<Vec<PolicyDefaultIntent>, NetworkError> {
        let attachments = self
            .inner
            .repository
            .list_endpoint_policy_attachments(project_id, &endpoint_id)
            .await
            .map_err(map_store_error)?;
        let mut defaults = Vec::new();
        for attachment in attachments.into_iter().filter(|a| a.state == "active") {
            let policy = self
                .inner
                .repository
                .get_reusable_policy(project_id, &attachment.policy_id)
                .await
                .map_err(map_store_error)?
                .ok_or(NetworkError::InvalidRequest)?;
            if policy.state != "active" || policy.stateful_mode != "Stateful" {
                return Err(NetworkError::InvalidRequest);
            }
            let unmatched_action = match policy.unmatched_action.as_str() {
                "Allow" => PolicyAction::Allow,
                "Deny" => PolicyAction::Deny,
                _ => return Err(NetworkError::InvalidRequest),
            };
            defaults.push(PolicyDefaultIntent {
                policy_id: policy.id,
                endpoint_id,
                unmatched_action,
                stateful_mode: PolicyStatefulMode::Stateful,
                generation: policy.generation.max(attachment.generation),
            });
        }
        defaults.sort_by_key(|default| default.policy_id);
        Ok(defaults)
    }

    /// Adds or replaces one canonical policy rule. NetworkIntent is not
    /// consulted or written; endpoint ownership establishes realm context.
    pub async fn upsert_policy_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        policy: PolicyIntent,
    ) -> Result<PolicyIntent, NetworkError> {
        let _guard = self.lock().await;
        if policy.endpoint_id.is_nil() {
            return Err(NetworkError::InvalidRequest);
        }
        validate_policy_shape(&policy)?;
        let endpoint = self
            .inner
            .repository
            .get_canonical_endpoint(project_id, &policy.endpoint_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &endpoint.realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::InvalidRequest)?;
        if realm.network_id != network_id || realm.state != "active" {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .upsert_canonical_policy(&canonical_policy_record(project_id, &policy))
            .await
            .map_err(map_store_error)?;
        Ok(policy)
    }

    pub async fn delete_policy_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        policy_id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        let exists = self
            .list_policies_for_project(project_id, network_id)
            .await?
            .iter()
            .any(|policy| policy.id == policy_id);
        if !exists {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .delete_canonical_policy(project_id, &policy_id)
            .await
            .map_err(map_store_error)
    }

    /// Compatibility hook retained for callers that report provider
    /// realization. Canonical Network state is authoritative; this hook must
    /// not mutate the transitional NetworkIntent payload.
    pub async fn mark_network_intent_active_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
    ) -> Result<(), NetworkError> {
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        Ok(())
    }

    pub async fn list_networks(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<NetworkRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListNetworks").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListNetworks".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        self.list_networks_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_networks_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkRecord>, NetworkError> {
        self.inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map(|networks| {
                networks
                    .into_iter()
                    .map(canonical_network_projection)
                    .collect()
            })
            .map_err(map_store_error)
    }

    pub async fn get_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<NetworkRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        self.get_network_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<NetworkRecord, NetworkError> {
        self.inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map(|network| network.map(canonical_network_projection))
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)
    }

    pub async fn update_network(
        &self,
        auth: &AuthContext,
        id: Uuid,
        name: Option<String>,
        admin_state_up: Option<bool>,
    ) -> Result<NetworkRecord, NetworkError> {
        let (namespace, action, resource_type) = self
            .authorize_canonical_action(auth, "UpdateNetwork", "network", Some(id), None)
            .await?;
        if name.as_deref().is_some_and(|value| value.trim().is_empty()) {
            return Err(NetworkError::InvalidRequest);
        }
        let project_id = auth.effective_scope().id().as_str();
        let current = self
            .inner
            .repository
            .get_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let name = name.unwrap_or_else(|| current.name.clone());
        let admin_state_up = admin_state_up.unwrap_or(current.admin_state_up);
        let result = self
            .inner
            .repository
            .update_canonical_network(project_id, &id, current.generation, &name, admin_state_up)
            .await
            .map(canonical_network_projection)
            .map_err(map_store_error);
        self.audit_canonical_result(
            auth,
            namespace,
            action,
            resource_type,
            Some(id),
            result.as_ref().map(|_| ()),
        );
        result
    }

    pub async fn delete_network(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeleteNetwork").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeleteNetwork".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "network")
                    .map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        match self
            .delete_network_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "network").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "network".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_network_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .delete_canonical_network(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self.inner.repository.delete_network(project_id, &id).await;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:network:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subnet(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
        cidr: String,
        gateway_ip: Option<Ipv4Addr>,
        allocation_start: Option<Ipv4Addr>,
        allocation_end: Option<Ipv4Addr>,
    ) -> Result<SubnetRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreateSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreateSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        match self
            .create_subnet_for_project(
                auth.effective_scope().id().as_str(),
                network_id,
                name,
                cidr,
                gateway_ip,
                allocation_start,
                allocation_end,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "subnet").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "subnet".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_subnet_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
        cidr: String,
        gateway_ip: Option<Ipv4Addr>,
        allocation_start: Option<Ipv4Addr>,
        allocation_end: Option<Ipv4Addr>,
    ) -> Result<SubnetRecord, NetworkError> {
        let net = Ipv4Net::parse(&cidr)?;
        let cidr = net.canonical();
        let gateway = gateway_ip.unwrap_or(net.first_host());
        if !net.contains(gateway) || gateway == net.network || gateway == net.broadcast {
            return Err(NetworkError::InvalidRequest);
        }
        let start = allocation_start.unwrap_or(Ipv4Addr::from(u32::from(net.first_host()) + 1));
        let end = allocation_end.unwrap_or(net.last_host());
        if !net.contains(start)
            || !net.contains(end)
            || start > end
            || (u32::from(start)..=u32::from(end)).contains(&u32::from(gateway))
        {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        let subnet = SubnetRecord {
            id: Uuid::now_v7(),
            network_id,
            name,
            project_id: project_id.to_owned(),
            cidr,
            gateway_ip: gateway,
            allocation_start: start,
            allocation_end: end,
            ip_version: 4,
            enable_dhcp: true,
        };
        let scope =
            OwnershipScope::project(ScopeId::new_unchecked(project_id.to_owned()), None, None);
        let amounts = vec![ResourceAmount::new(LimitKey::network_subnets(), 1)];
        let op_id = format!("o3k:subnet:create:{}:{}", project_id, subnet.id);
        let quota_res = self
            .inner
            .repository
            .reserve_quota(&scope, &op_id, &amounts)
            .await
            .map_err(|err| match err {
                o3k_store::StoreError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                } => NetworkError::QuotaExceeded {
                    key,
                    limit,
                    used,
                    requested,
                },
                o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
                other => map_store_error(other),
            })?;

        if self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?
            .iter()
            .any(|realm| realm.state == "active")
        {
            let _ = self
                .inner
                .repository
                .release_reservation(&quota_res.id)
                .await;
            return Err(NetworkError::Conflict);
        }

        let realm = o3k_store::CanonicalAddressRealmRecord {
            id: subnet.id,
            network_id: subnet.network_id,
            project_id: subnet.project_id.clone(),
            prefix: subnet.cidr.clone(),
            overlapping_prefixes: false,
            generation: 1,
            state: "active".to_owned(),
        };
        let pool = o3k_store::CanonicalAddressPoolRecord {
            id: Uuid::now_v7(),
            realm_id: realm.id,
            project_id: realm.project_id.clone(),
            prefix: realm.prefix.clone(),
            gateway: Some(subnet.gateway_ip),
            first_usable: subnet.allocation_start,
            last_usable: subnet.allocation_end,
            generation: 1,
            state: "active".to_owned(),
        };
        match self
            .inner
            .repository
            .insert_subnet_bundle(&realm, &pool, &subnet)
            .await
        {
            Ok(()) => {
                let _ = self
                    .inner
                    .repository
                    .commit_reservation(&quota_res.id)
                    .await;
                Ok(subnet)
            }
            Err(o3k_store::StoreError::NetworkInUse)
            | Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(NetworkError::Conflict)
            }
            Err(error) => {
                let _ = self
                    .inner
                    .repository
                    .release_reservation(&quota_res.id)
                    .await;
                Err(map_store_error(error))
            }
        }
    }

    pub async fn list_subnets(
        &self,
        auth: &AuthContext,
    ) -> Result<Vec<SubnetRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListSubnets").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListSubnets".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        self.list_subnets_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_subnets_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<SubnetRecord>, NetworkError> {
        let networks = self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?;
        let mut result = Vec::new();
        for network in networks {
            for realm in self
                .inner
                .repository
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(map_store_error)?
            {
                result.push(self.project_canonical_subnet(project_id, &realm).await?);
            }
        }
        Ok(result)
    }

    pub async fn get_subnet(
        &self,
        auth: &AuthContext,
        id: Uuid,
    ) -> Result<SubnetRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        self.get_subnet_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<SubnetRecord, NetworkError> {
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.project_canonical_subnet(project_id, &realm).await
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn update_subnet(
        &self,
        auth: &AuthContext,
        id: Uuid,
        name: Option<String>,
        gateway_ip: Option<Ipv4Addr>,
        enable_dhcp: Option<bool>,
        network_id: Option<Uuid>,
        cidr: Option<String>,
        ip_version: Option<u8>,
    ) -> Result<SubnetRecord, NetworkError> {
        let action = ActionId::new("network", "UpdateSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "UpdateSubnet".to_owned())
        });
        let request = AuthorizationRequest {
            auth_context: auth,
            action,
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        if !self.authorizer.authorize(&request).is_allowed() {
            return Err(NetworkError::NotFound);
        }
        if network_id.is_some() || cidr.is_some() || ip_version.is_some_and(|v| v != 4) {
            return Err(NetworkError::InvalidRequest);
        }
        self.update_subnet_for_project(
            auth.effective_scope().id().as_str(),
            id,
            name,
            gateway_ip,
            enable_dhcp,
        )
        .await
    }

    async fn update_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: Option<String>,
        gateway_ip: Option<Ipv4Addr>,
        enable_dhcp: Option<bool>,
    ) -> Result<SubnetRecord, NetworkError> {
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::InvalidRequest)?;
        let current = self.project_canonical_subnet(project_id, &realm).await?;
        let gateway = gateway_ip.unwrap_or(current.gateway_ip);
        let net = Ipv4Net::parse(&realm.prefix)?;
        if !net.contains(gateway) || gateway == net.network || gateway == net.broadcast {
            return Err(NetworkError::InvalidRequest);
        }
        if (u32::from(pool.first_usable)..=u32::from(pool.last_usable))
            .contains(&u32::from(gateway))
        {
            return Err(NetworkError::Conflict);
        }
        let updated = SubnetRecord {
            name: name.unwrap_or(current.name),
            gateway_ip: gateway,
            enable_dhcp: enable_dhcp.unwrap_or(current.enable_dhcp),
            ..current
        };
        if gateway != pool.gateway.unwrap_or(gateway) {
            self.inner
                .repository
                .update_subnet_bundle(&updated, &pool.id, pool.generation)
                .await
                .map_err(map_store_error)?;
        } else {
            self.inner
                .repository
                .update_subnet(&updated)
                .await
                .map_err(map_store_error)?;
        }
        self.get_subnet_for_project(project_id, id).await
    }

    async fn project_canonical_subnet(
        &self,
        project_id: &str,
        realm: &o3k_store::CanonicalAddressRealmRecord,
    ) -> Result<SubnetRecord, NetworkError> {
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &realm.id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::InvalidRequest)?;
        let metadata = self
            .inner
            .repository
            .get_subnet(project_id, &realm.id)
            .await
            .map_err(map_store_error)?;
        Ok(SubnetRecord {
            id: realm.id,
            network_id: realm.network_id,
            name: metadata
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_default(),
            project_id: realm.project_id.clone(),
            cidr: realm.prefix.clone(),
            gateway_ip: pool.gateway.ok_or(NetworkError::InvalidRequest)?,
            allocation_start: pool.first_usable,
            allocation_end: pool.last_usable,
            ip_version: 4,
            enable_dhcp: metadata
                .as_ref()
                .map(|value| value.enable_dhcp)
                .unwrap_or(true),
        })
    }

    pub async fn delete_subnet(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeleteSubnet").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeleteSubnet".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "subnet").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        match self
            .delete_subnet_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "subnet").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "subnet".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_subnet_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        let _guard = self.lock().await;
        let metadata_exists = self
            .inner
            .repository
            .get_subnet(project_id, &id)
            .await
            .map_err(map_store_error)?
            .is_some();
        let realm_exists = self
            .inner
            .repository
            .get_canonical_realm(project_id, &id)
            .await
            .map_err(map_store_error)?
            .is_some();
        if !metadata_exists && !realm_exists {
            return Err(NetworkError::NotFound);
        }
        self.inner
            .repository
            .delete_subnet_bundle(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:subnet:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    pub async fn create_port(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        self.create_port_with_fixed_ip(auth, network_id, name, None)
            .await
    }

    pub async fn create_port_with_fixed_ip(
        &self,
        auth: &AuthContext,
        network_id: Uuid,
        name: String,
        requested_fixed_ip: Option<(Uuid, Option<Ipv4Addr>)>,
    ) -> Result<PortRecord, NetworkError> {
        if name.starts_with("o3k-server:") {
            return Err(NetworkError::InvalidRequest);
        }
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "CreatePort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "CreatePort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        match self
            .create_port_for_project_with_fixed_ip(
                auth.effective_scope().id().as_str(),
                network_id,
                name,
                requested_fixed_ip,
            )
            .await
        {
            Ok(record) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "port").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "port".to_owned())
                        }),
                        ResourceId::new(record.id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(record)
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn create_port_for_project(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        self.create_port_for_project_with_fixed_ip(project_id, network_id, name, None)
            .await
    }

    pub async fn create_port_for_project_with_fixed_ip(
        &self,
        project_id: &str,
        network_id: Uuid,
        name: String,
        requested_fixed_ip: Option<(Uuid, Option<Ipv4Addr>)>,
    ) -> Result<PortRecord, NetworkError> {
        self.get_canonical_network_for_project(project_id, network_id)
            .await?;
        let realms = self
            .inner
            .repository
            .list_canonical_realms(project_id, &network_id)
            .await
            .map_err(map_store_error)?;
        let realm = if let Some((subnet_id, _)) = requested_fixed_ip {
            realms
                .into_iter()
                .find(|realm| realm.id == subnet_id && realm.state == "active")
                .ok_or(NetworkError::NotFound)?
        } else {
            match realms.as_slice() {
                [] => return Err(NetworkError::NotFound),
                [realm] if realm.state == "active" => realm.clone(),
                [_] => return Err(NetworkError::Conflict),
                _ => return Err(NetworkError::InvalidRequest),
            }
        };
        let pool = self
            .inner
            .repository
            .list_canonical_pools(project_id, &realm.id)
            .await
            .map_err(map_store_error)?
            .into_iter()
            .next()
            .ok_or(NetworkError::NotFound)?;
        let explicit_ip = requested_fixed_ip.and_then(|(_, ip)| ip);
        let mut candidate = explicit_ip
            .map(u32::from)
            .unwrap_or_else(|| u32::from(pool.first_usable));
        let end = explicit_ip
            .map(u32::from)
            .unwrap_or_else(|| u32::from(pool.last_usable));
        let gateway = pool.gateway.ok_or(NetworkError::InvalidRequest)?;
        while candidate <= end {
            let address = Ipv4Addr::from(candidate);
            if address != gateway
                && candidate >= u32::from(pool.first_usable)
                && candidate <= u32::from(pool.last_usable)
            {
                let id = Uuid::now_v7();
                let port = PortRecord {
                    id,
                    network_id,
                    subnet_id: Some(realm.id),
                    project_id: project_id.to_owned(),
                    name: name.clone(),
                    mac_address: deterministic_port_mac(id),
                    fixed_ip: address,
                    status: "ACTIVE".to_owned(),
                    binding_host: None,
                    binding_state: None,
                };
                let scope = OwnershipScope::project(
                    ScopeId::new_unchecked(project_id.to_owned()),
                    None,
                    None,
                );
                let amounts = vec![ResourceAmount::new(LimitKey::network_ports(), 1)];
                let op_id = format!("o3k:port:create:{}:{}", project_id, port.id);
                let quota_res = self
                    .inner
                    .repository
                    .reserve_quota(&scope, &op_id, &amounts)
                    .await
                    .map_err(|err| match err {
                        o3k_store::StoreError::QuotaExceeded {
                            key,
                            limit,
                            used,
                            requested,
                        } => NetworkError::QuotaExceeded {
                            key,
                            limit,
                            used,
                            requested,
                        },
                        o3k_store::StoreError::ReservationConflict(_) => NetworkError::Conflict,
                        other => map_store_error(other),
                    })?;

                let endpoint = o3k_store::CanonicalEndpointRecord {
                    id: port.id,
                    realm_id: realm.id,
                    project_id: project_id.to_owned(),
                    fixed_ip: port.fixed_ip,
                    mac: port.mac_address.clone(),
                    generation: 1,
                    state: "active".to_owned(),
                };
                let mut insert_result = Err(o3k_store::StoreError::ResourceNotFound);
                for _ in 0..8 {
                    insert_result = self
                        .inner
                        .repository
                        .insert_canonical_endpoint_and_port(&endpoint, &port)
                        .await;
                    if !insert_result
                        .as_ref()
                        .is_err_and(|error| error.to_string().contains("database is locked"))
                    {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                match insert_result {
                    Ok(()) => {
                        let _ = self
                            .inner
                            .repository
                            .commit_reservation(&quota_res.id)
                            .await;
                        return Ok(port);
                    }
                    Err(o3k_store::StoreError::ResourceAlreadyExists) => {
                        let _ = self
                            .inner
                            .repository
                            .release_reservation(&quota_res.id)
                            .await;
                        if explicit_ip.is_some() {
                            return Err(NetworkError::Conflict);
                        }
                    }
                    Err(error) => {
                        let _ = self
                            .inner
                            .repository
                            .release_reservation(&quota_res.id)
                            .await;
                        return Err(map_store_error(error));
                    }
                }
            }
            if explicit_ip.is_some() {
                break;
            }
            candidate = candidate.saturating_add(1);
        }
        if explicit_ip.is_some() {
            Err(NetworkError::InvalidRequest)
        } else {
            Err(NetworkError::PoolExhausted)
        }
    }

    pub async fn list_ports(&self, auth: &AuthContext) -> Result<Vec<PortRecord>, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ListPorts").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ListPorts".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::collection(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::Unauthorized);
        }
        self.list_ports_for_project(auth.effective_scope().id().as_str())
            .await
    }

    pub async fn list_ports_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<PortRecord>, NetworkError> {
        let networks = self
            .inner
            .repository
            .list_canonical_networks(project_id)
            .await
            .map_err(map_store_error)?;
        let mut result = Vec::new();
        for network in networks {
            for realm in self
                .inner
                .repository
                .list_canonical_realms(project_id, &network.id)
                .await
                .map_err(map_store_error)?
            {
                for endpoint in self
                    .inner
                    .repository
                    .list_canonical_endpoints(project_id, &realm.id)
                    .await
                    .map_err(map_store_error)?
                {
                    result.push(
                        self.project_canonical_port(project_id, &realm, &endpoint)
                            .await?,
                    );
                }
            }
        }
        Ok(result)
    }

    pub async fn get_port(&self, auth: &AuthContext, id: Uuid) -> Result<PortRecord, NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "ReadPort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "ReadPort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        self.get_port_for_project(auth.effective_scope().id().as_str(), id)
            .await
    }

    pub async fn get_port_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<PortRecord, NetworkError> {
        let endpoint = self
            .inner
            .repository
            .get_canonical_endpoint(project_id, &id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let realm = self
            .inner
            .repository
            .get_canonical_realm(project_id, &endpoint.realm_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.project_canonical_port(project_id, &realm, &endpoint)
            .await
    }

    pub async fn update_port_name_for_project(
        &self,
        project_id: &str,
        id: Uuid,
        name: String,
    ) -> Result<PortRecord, NetworkError> {
        let current = self.get_port_for_project(project_id, id).await?;
        if current.name.starts_with("o3k-server:") {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .update_port_name(project_id, &id, &name)
            .await
            .map_err(map_store_error)?;
        self.get_port_for_project(project_id, id).await
    }

    async fn project_canonical_port(
        &self,
        project_id: &str,
        realm: &o3k_store::CanonicalAddressRealmRecord,
        endpoint: &o3k_store::CanonicalEndpointRecord,
    ) -> Result<PortRecord, NetworkError> {
        let metadata = self
            .inner
            .repository
            .get_port(project_id, &endpoint.id)
            .await
            .map_err(map_store_error)?;
        Ok(PortRecord {
            id: endpoint.id,
            network_id: realm.network_id,
            subnet_id: Some(realm.id),
            project_id: endpoint.project_id.clone(),
            name: metadata
                .as_ref()
                .map(|value| value.name.clone())
                .unwrap_or_default(),
            mac_address: endpoint.mac.clone(),
            fixed_ip: endpoint.fixed_ip,
            status: endpoint.state.to_ascii_uppercase(),
            binding_host: metadata
                .as_ref()
                .and_then(|value| value.binding_host.clone()),
            binding_state: metadata.and_then(|value| value.binding_state),
        })
    }

    /// Internal owner lookup used by canonical dependency authorization. It
    /// is not exposed as a tenant-facing read path and carries no metadata to
    /// the caller beyond the durable owner record.
    pub async fn find_port_by_id(&self, id: Uuid) -> Result<Option<PortRecord>, NetworkError> {
        self.inner
            .repository
            .get_port_by_id(&id)
            .await
            .map_err(map_store_error)
    }

    pub async fn delete_port(&self, auth: &AuthContext, id: Uuid) -> Result<(), NetworkError> {
        let ns = ServiceNamespace::new("network")
            .unwrap_or_else(|_| ServiceNamespace::new_unchecked("network".to_owned()));
        let act = ActionId::new("network", "DeletePort").unwrap_or_else(|_| {
            ActionId::new_unchecked("network".to_owned(), "DeletePort".to_owned())
        });
        let req = AuthorizationRequest {
            auth_context: auth,
            action: act.clone(),
            resource_target: ResourceTarget::instance(
                ResourceType::new("network", "port").map_err(|_| NetworkError::InvalidRequest)?,
                ResourceId::new(id.to_string()).map_err(|_| NetworkError::InvalidRequest)?,
                Some(auth.effective_scope().id().clone()),
            ),
        };
        let decision = self.authorizer.authorize(&req);
        if !decision.is_allowed() {
            let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Denied)
                .with_decision(decision)
                .with_reason("unauthorized");
            self.audit_sink.record(&event);
            return Err(NetworkError::NotFound);
        }
        match self
            .delete_port_for_project(auth.effective_scope().id().as_str(), id)
            .await
        {
            Ok(()) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Succeeded)
                    .with_resource(
                        ResourceType::new("network", "port").unwrap_or_else(|_| {
                            ResourceType::new_unchecked("network".to_owned(), "port".to_owned())
                        }),
                        ResourceId::new(id.to_string()).ok(),
                        Some(auth.effective_scope().clone()),
                    );
                self.audit_sink.record(&event);
                Ok(())
            }
            Err(error) => {
                let event = AuditEvent::from_auth(auth, ns, act, AuditOutcome::Failed)
                    .with_reason(error.to_string());
                self.audit_sink.record(&event);
                Err(error)
            }
        }
    }

    pub async fn delete_port_for_project(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), NetworkError> {
        // Endpoint deletion owns only the endpoint and its canonical
        // attachment relations.  Remove those relations explicitly before
        // the endpoint row so the reusable policy and its rules remain
        // independent and the endpoint delete cannot leave dangling
        // attachments.
        let attachments = self
            .inner
            .repository
            .list_endpoint_policy_attachments(project_id, &id)
            .await
            .map_err(map_store_error)?;
        for attachment in attachments {
            self.inner
                .repository
                .delete_policy_attachment(project_id, &attachment.id)
                .await
                .map_err(map_store_error)?;
        }
        self.inner
            .repository
            .delete_canonical_endpoint_and_port(project_id, &id)
            .await
            .map_err(map_store_error)?;
        let _ = self
            .inner
            .repository
            .release_reservation_for_operation(&format!("o3k:port:create:{}:{}", project_id, id))
            .await;
        Ok(())
    }

    pub async fn record_binding_intent(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
    ) -> Result<PortRecord, NetworkError> {
        if host.trim().is_empty() {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port
            .binding_host
            .as_deref()
            .is_some_and(|current| current != host)
        {
            return Err(NetworkError::Conflict);
        }
        // A create dispatch is underway: transitions from unbound, binding,
        // down, and error to binding. A completed `bound` observation is kept:
        // idempotent dispatch replays of an already-succeeded create must not
        // downgrade durable observed state.
        let next = match port
            .binding_state
            .as_deref()
            .and_then(PortBindingState::parse)
        {
            Some(PortBindingState::Bound) => PortBindingState::Bound,
            _ => PortBindingState::Binding,
        };
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(next.as_str()))
            .await
            .map_err(map_store_error)
    }

    pub async fn project_binding_observation(
        &self,
        project_id: &str,
        port_id: Uuid,
        host: &str,
        state: &str,
    ) -> Result<PortRecord, NetworkError> {
        let state = PortBindingState::parse(state).ok_or(NetworkError::InvalidRequest)?;
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        if port.binding_host.as_deref() != Some(host) {
            return Err(NetworkError::Conflict);
        }
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(state.as_str()))
            .await
            .map_err(map_store_error)
    }

    /// Projects a terminal create outcome onto the port's binding using the
    /// host recorded by the dispatch intent. The durable intent is
    /// authoritative: the control plane selects the host, so a stale or
    /// mismatched caller identity cannot override it. A port without a
    /// recorded intent (never dispatched) rejects the projection.
    pub async fn project_create_outcome(
        &self,
        project_id: &str,
        port_id: Uuid,
        state: PortBindingState,
    ) -> Result<PortRecord, NetworkError> {
        if !matches!(state, PortBindingState::Bound | PortBindingState::Error) {
            return Err(NetworkError::InvalidRequest);
        }
        let _guard = self.lock().await;
        let port = self
            .inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        let host = port.binding_host.as_deref().ok_or(NetworkError::Conflict)?;
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, Some(host), Some(state.as_str()))
            .await
            .map_err(map_store_error)
    }

    /// Clears the binding of a port whose server reached terminal deletion.
    /// Idempotent: unbinding a port with no intent is a successful no-op.
    pub async fn unbind_port(
        &self,
        project_id: &str,
        port_id: Uuid,
    ) -> Result<PortRecord, NetworkError> {
        let _guard = self.lock().await;
        self.inner
            .repository
            .get_port(project_id, &port_id)
            .await
            .map_err(map_store_error)?
            .ok_or(NetworkError::NotFound)?;
        self.inner
            .repository
            .update_port_binding(project_id, &port_id, None, None)
            .await
            .map_err(map_store_error)
    }

    async fn lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.lock.lock().await
    }
}

#[derive(Clone, Copy)]
struct Ipv4Net {
    network: Ipv4Addr,
    broadcast: Ipv4Addr,
    prefix: u8,
}

impl Ipv4Net {
    fn parse(value: &str) -> Result<Self, NetworkError> {
        let (address, prefix) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
        let address: Ipv4Addr = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
        let prefix: u8 = prefix.parse().map_err(|_| NetworkError::InvalidRequest)?;
        if prefix > 30 {
            return Err(NetworkError::InvalidRequest);
        }
        let raw = u32::from(address);
        let mask = if prefix == 0 {
            0
        } else {
            u32::MAX << (32 - prefix)
        };
        let network = Ipv4Addr::from(raw & mask);
        let broadcast = Ipv4Addr::from((raw & mask) | !mask);
        Ok(Self {
            network,
            broadcast,
            prefix,
        })
    }

    fn canonical(self) -> String {
        format!("{}/{}", self.network, self.prefix)
    }

    fn contains(self, address: Ipv4Addr) -> bool {
        let raw = u32::from(address);
        raw >= u32::from(self.network) && raw <= u32::from(self.broadcast)
    }
    fn first_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.network) + 1)
    }
    fn last_host(self) -> Ipv4Addr {
        Ipv4Addr::from(u32::from(self.broadcast) - 1)
    }
}

/// The legacy `metadata.json` shape written by previous versions. It is
/// parsed once, imported into the durable store, and the file is renamed so
/// it is never read again.
#[derive(serde::Deserialize)]
struct LegacyFile {
    networks: Vec<LegacyNetwork>,
    subnets: Vec<LegacySubnet>,
    ports: Vec<LegacyPort>,
}

#[derive(serde::Deserialize)]
struct LegacyNetwork {
    id: Uuid,
    name: String,
    project_id: String,
    status: String,
}

#[derive(serde::Deserialize)]
struct LegacySubnet {
    id: Uuid,
    network_id: Uuid,
    name: String,
    project_id: String,
    cidr: String,
    gateway_ip: Ipv4Addr,
    allocation_start: Ipv4Addr,
    allocation_end: Ipv4Addr,
}

#[derive(serde::Deserialize)]
struct LegacyPort {
    id: Uuid,
    network_id: Uuid,
    #[serde(default)]
    subnet_id: Uuid,
    project_id: String,
    name: String,
    #[serde(default)]
    mac_address: String,
    fixed_ip: Ipv4Addr,
    status: String,
}

/// Imports the legacy `metadata.json` file exactly once, in dependency order
/// (networks, then subnets, then ports), and renames it so `open` never reads
/// it again. The rename is best-effort: when it fails, the next `open`
/// re-reads the file, but the import is idempotent (records already present
/// are skipped), so the file can never double-import. Inserts skip records
/// that are already present, which makes a partially completed previous
/// import crash-resume safe. A corrupt file, duplicate MACs, or any
/// non-already-exists insert error fails the import closed and leaves the
/// file in place.
async fn import_legacy_metadata(
    root: &Path,
    repository: &dyn o3k_store::NetworkRepository,
) -> Result<(), NetworkError> {
    let path = root.join("metadata.json");
    let file = fs::File::open(&path)
        .map_err(|error| NetworkError::CorruptMetadata(serde_json::Error::io(error)))?;
    let mut legacy: LegacyFile =
        serde_json::from_reader(file).map_err(NetworkError::CorruptMetadata)?;
    let mut macs = HashSet::new();
    for port in &mut legacy.ports {
        if port.mac_address.is_empty() {
            port.mac_address = deterministic_port_mac(port.id);
        }
        if port.subnet_id.is_nil()
            && let Some(subnet) = legacy.subnets.iter().find(|subnet| {
                subnet.network_id == port.network_id && subnet.project_id == port.project_id
            })
        {
            port.subnet_id = subnet.id;
        }
        if !macs.insert(port.mac_address.to_ascii_lowercase()) {
            return Err(NetworkError::Conflict);
        }
    }
    for network in &legacy.networks {
        let record = NetworkRecord {
            id: network.id,
            name: network.name.clone(),
            project_id: network.project_id.clone(),
            status: network.status.clone(),
        };
        match repository.insert_network(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for subnet in &legacy.subnets {
        let record = SubnetRecord {
            id: subnet.id,
            network_id: subnet.network_id,
            name: subnet.name.clone(),
            project_id: subnet.project_id.clone(),
            cidr: subnet.cidr.clone(),
            gateway_ip: subnet.gateway_ip,
            allocation_start: subnet.allocation_start,
            allocation_end: subnet.allocation_end,
            ip_version: 4,
            enable_dhcp: true,
        };
        match repository.insert_subnet(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    for port in &legacy.ports {
        let record = PortRecord {
            id: port.id,
            network_id: port.network_id,
            subnet_id: (!port.subnet_id.is_nil()).then_some(port.subnet_id),
            project_id: port.project_id.clone(),
            name: port.name.clone(),
            mac_address: port.mac_address.clone(),
            fixed_ip: port.fixed_ip,
            status: port.status.clone(),
            binding_host: None,
            binding_state: None,
        };
        match repository.insert_port(&record).await {
            Ok(()) | Err(o3k_store::StoreError::ResourceAlreadyExists) => {}
            Err(error) => return Err(map_store_error(error)),
        }
    }
    let _ = fs::rename(&path, root.join("metadata.json.imported"));
    Ok(())
}

pub(crate) fn parse_security_group_prefix(value: &str) -> Result<Ipv4Prefix, NetworkError> {
    let (address, length) = value.split_once('/').ok_or(NetworkError::InvalidRequest)?;
    let address = address.parse().map_err(|_| NetworkError::InvalidRequest)?;
    let length = length.parse().map_err(|_| NetworkError::InvalidRequest)?;
    Ipv4Prefix::new(address, length).ok_or(NetworkError::InvalidRequest)
}

pub(crate) fn parse_security_group_direction(value: &str) -> Result<PolicyDirection, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "ingress" => Ok(PolicyDirection::Ingress),
        "egress" => Ok(PolicyDirection::Egress),
        _ => Err(NetworkError::InvalidRequest),
    }
}

pub(crate) fn parse_security_group_protocol(value: &str) -> Result<NetworkProtocol, NetworkError> {
    match value.to_ascii_lowercase().as_str() {
        "any" => Ok(NetworkProtocol::Any),
        "tcp" => Ok(NetworkProtocol::Tcp),
        "udp" => Ok(NetworkProtocol::Udp),
        "icmp" => Ok(NetworkProtocol::Icmp),
        _ => Err(NetworkError::InvalidRequest),
    }
}

fn deterministic_port_mac(port_id: Uuid) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(port_id.as_bytes());
    format!(
        "02:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        digest[0], digest[1], digest[2], digest[3], digest[4]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use o3k_domain::PolicyAction;
    use o3k_store::DurableStore;

    fn auth(project_id: &str) -> AuthContext {
        AuthContext::new(
            o3k_kernel::Principal::User(o3k_kernel::UserPrincipal::new(
                o3k_kernel::PrincipalId::new_unchecked("test-user"),
                "test-user",
                Some("default".to_string()),
            )),
            o3k_kernel::OwnershipScope::project(
                o3k_kernel::ScopeId::new_unchecked(project_id),
                Some(project_id.to_string()),
                Some("default".to_string()),
            ),
            vec!["admin".to_string()],
            1000,
            5000,
            uuid::Uuid::now_v7().to_string(),
            uuid::Uuid::now_v7().to_string(),
            None,
        )
    }

    fn root(label: &str) -> PathBuf {
        PathBuf::from(format!("/tmp/o3k-network-{label}-{}", std::process::id()))
    }

    #[tokio::test]
    async fn canonical_service_reconstructs_zero_and_multiple_realms()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-runtime");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "canonical".to_owned())
            .await?;
        let empty = service
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert!(empty.realms.is_empty());

        let realm_a = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.0.0.0/24".to_owned(),
                true,
            )
            .await?;
        let realm_b = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.0.0.0/24".to_owned(),
                true,
            )
            .await?;
        let realm_c = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.1.0.0/24".to_owned(),
                false,
            )
            .await?;
        let pool = service
            .create_canonical_pool_for_project(
                "project-a",
                realm_a.id,
                "10.0.0.0/24".to_owned(),
                Some("10.0.0.1".parse()?),
                "10.0.0.2".parse()?,
                "10.0.0.254".parse()?,
            )
            .await?;
        let endpoint_a = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm_a.id,
                "10.0.0.10".parse()?,
                "02:00:00:00:00:10".to_owned(),
            )
            .await?;
        let endpoint_b = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm_b.id,
                "10.0.0.10".parse()?,
                "02:00:00:00:00:11".to_owned(),
            )
            .await?;
        assert_eq!(endpoint_a.fixed_ip, endpoint_b.fixed_ip);
        assert!(matches!(
            service
                .create_canonical_endpoint_for_project(
                    "project-a",
                    realm_a.id,
                    endpoint_a.fixed_ip,
                    "02:00:00:00:00:12".to_owned()
                )
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm_a.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        drop(service);
        drop(store);

        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let snapshot = reopened
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert_eq!(snapshot.network.id, network.id);
        assert_eq!(snapshot.realms.len(), 3);
        assert_eq!(snapshot.pools[&realm_a.id], vec![pool]);
        assert_eq!(snapshot.endpoints[&realm_a.id], vec![endpoint_a]);
        reopened
            .delete_canonical_realm_for_project("project-a", realm_c.id)
            .await?;
        assert_eq!(
            reopened
                .reconstruct_canonical_network("project-a", network.id)
                .await?
                .realms
                .len(),
            2
        );
        assert!(matches!(
            reopened
                .delete_canonical_network_for_project("project-a", network.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn network_rename_updates_projection_and_reopens_with_new_name()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("rename-restart");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let identity = auth("project-a");
        let network = service
            .create_network(&identity, "before".to_owned())
            .await?;
        let renamed = service
            .update_network(&identity, network.id, Some("after".to_owned()), Some(false))
            .await?;
        assert_eq!(renamed.id, network.id);
        assert_eq!(renamed.name, "after");
        let canonical = store
            .get_canonical_network("project-a", &network.id)
            .await?
            .ok_or("canonical network after rename")?;
        assert!(!canonical.admin_state_up);
        assert_eq!(
            store
                .get_network("project-a", &network.id)
                .await?
                .map(|n| n.name),
            Some("after".to_owned())
        );

        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_network(&identity, network.id).await?;
        assert_eq!(restored.id, network.id);
        assert_eq!(restored.project_id, "project-a");
        assert_eq!(restored.name, "after");
        let restored_canonical = reopened_store
            .get_canonical_network("project-a", &network.id)
            .await?
            .ok_or("canonical network after restart")?;
        assert!(!restored_canonical.admin_state_up);
        assert_eq!(
            reopened_store
                .get_network("project-a", &network.id)
                .await?
                .map(|n| n.name),
            Some("after".to_owned())
        );
        assert!(
            reopened_store
                .get_network("project-a", &network.id)
                .await?
                .is_some()
        );
        Ok(())
    }

    #[tokio::test]
    async fn canonical_reads_do_not_require_projection_metadata()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-reads");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "canonical".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.20.0.0/24".to_owned(),
                false,
            )
            .await?;
        let _pool = service
            .create_canonical_pool_for_project(
                "project-a",
                realm.id,
                "10.20.0.0/24".to_owned(),
                Some("10.20.0.1".parse()?),
                "10.20.0.2".parse()?,
                "10.20.0.254".parse()?,
            )
            .await?;
        let endpoint = service
            .create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.20.0.10".parse()?,
                "02:00:00:20:00:10".to_owned(),
            )
            .await?;

        let subnet = service
            .get_subnet_for_project("project-a", realm.id)
            .await?;
        assert_eq!(subnet.id, realm.id);
        assert_eq!(subnet.network_id, network.id);
        assert!(subnet.name.is_empty());

        let port = service
            .get_port_for_project("project-a", endpoint.id)
            .await?;
        assert_eq!(port.id, endpoint.id);
        assert_eq!(port.subnet_id, Some(realm.id));
        assert_eq!(port.fixed_ip, endpoint.fixed_ip);
        assert_eq!(port.mac_address, endpoint.mac);
        assert!(port.name.is_empty());

        drop(service);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_canonical_entry_points_enforce_scope_and_audit()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-auth");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let sink = Arc::new(o3k_kernel::MemoryAuditSink::new());
        let service = NetworkService::open(&path, store)
            .await?
            .with_audit_sink(sink.clone());
        let network = service
            .create_canonical_network(&auth("project-a"), "authorized".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm(
                &auth("project-a"),
                network.id,
                "10.30.0.0/24".to_owned(),
                false,
            )
            .await?;
        assert!(matches!(
            service
                .delete_canonical_realm(&auth("project-b"), realm.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let events = sink.events();
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:DeleteAddressRealm"
                && event.outcome == AuditOutcome::Denied
                && event
                    .resource_id
                    .as_ref()
                    .is_some_and(|id| id.as_str() == realm.id.to_string())
        }));
        Ok(())
    }

    #[tokio::test]
    async fn authenticated_parent_actions_use_canonical_owner_and_audit_outcomes()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-auth-matrix");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let sink = Arc::new(o3k_kernel::MemoryAuditSink::new());
        let service = NetworkService::open(&path, store)
            .await?
            .with_audit_sink(sink.clone());
        let network = service
            .create_canonical_network(&auth("project-a"), "matrix".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm(
                &auth("project-a"),
                network.id,
                "10.32.0.0/24".to_owned(),
                false,
            )
            .await?;

        assert!(matches!(
            service
                .create_canonical_realm(
                    &auth("project-b"),
                    network.id,
                    "10.33.0.0/24".to_owned(),
                    false,
                )
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_pool(
                    &auth("project-b"),
                    realm.id,
                    "10.32.0.0/24".to_owned(),
                    Some("10.32.0.1".parse()?),
                    "10.32.0.2".parse()?,
                    "10.32.0.254".parse()?,
                )
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_endpoint(
                    &auth("project-b"),
                    realm.id,
                    "10.32.0.10".parse()?,
                    "02:00:00:32:00:10".to_owned(),
                )
                .await,
            Err(NetworkError::NotFound)
        ));

        let pool = service
            .create_canonical_pool(
                &auth("project-a"),
                realm.id,
                "10.32.0.0/24".to_owned(),
                Some("10.32.0.1".parse()?),
                "10.32.0.2".parse()?,
                "10.32.0.254".parse()?,
            )
            .await?;
        assert_eq!(
            service
                .list_canonical_pools(&auth("project-a"), realm.id)
                .await?
                .len(),
            1
        );
        assert!(matches!(
            service
                .list_canonical_pools(&auth("project-b"), realm.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let endpoint = service
            .create_canonical_endpoint(
                &auth("project-a"),
                realm.id,
                "10.32.0.10".parse()?,
                "02:00:00:32:00:10".to_owned(),
            )
            .await?;
        assert_eq!(
            service
                .get_canonical_realm(&auth("project-a"), realm.id)
                .await?
                .id,
            realm.id
        );
        assert_eq!(
            service
                .list_canonical_endpoints(&auth("project-a"), realm.id)
                .await?
                .len(),
            1
        );
        assert_eq!(
            service
                .get_canonical_endpoint(&auth("project-a"), endpoint.id)
                .await?
                .id,
            endpoint.id
        );
        assert!(matches!(
            service
                .get_canonical_endpoint(&auth("project-b"), endpoint.id)
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .create_canonical_network(&auth("project-a"), "matrix".to_owned())
                .await,
            Err(NetworkError::Conflict)
        ));

        let events = sink.events();
        let denied = events
            .iter()
            .filter(|event| event.outcome == AuditOutcome::Denied)
            .collect::<Vec<_>>();
        assert!(denied.len() >= 3);
        assert!(denied.iter().all(|event| {
            event
                .authorization_decision
                .as_ref()
                .is_some_and(|decision| {
                    decision.reason() == &o3k_kernel::DecisionReason::ScopeMismatch
                })
                && event
                    .owner_scope
                    .as_ref()
                    .is_some_and(|scope| scope.id().as_str() == "project-a")
        }));
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:CreateNetwork"
                && event.outcome == AuditOutcome::Failed
        }));
        assert!(events.iter().any(|event| {
            event.action.to_string() == "network:CreateEndpoint"
                && event.outcome == AuditOutcome::Succeeded
        }));
        service
            .delete_canonical_endpoint(&auth("project-a"), endpoint.id)
            .await?;
        service
            .delete_canonical_pool(&auth("project-a"), pool.id)
            .await?;
        service
            .delete_canonical_realm(&auth("project-a"), realm.id)
            .await?;
        service
            .delete_canonical_network(&auth("project-a"), network.id)
            .await?;
        let final_events = sink.events();
        assert!(final_events.iter().any(|event| {
            event.action.to_string() == "network:DeleteNetwork"
                && event.outcome == AuditOutcome::Succeeded
        }));
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn independent_services_preserve_canonical_endpoint_and_realm_races()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-races");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store_a = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let network = service_a
            .create_canonical_network_for_project("project-a", "races".to_owned())
            .await?;
        let realm = service_a
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.31.0.0/24".to_owned(),
                false,
            )
            .await?;
        let (left, right) = tokio::join!(
            service_a.create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.31.0.10".parse()?,
                "02:00:00:00:31:10".to_owned(),
            ),
            service_b.create_canonical_endpoint_for_project(
                "project-a",
                realm.id,
                "10.31.0.10".parse()?,
                "02:00:00:00:31:11".to_owned(),
            )
        );
        assert_eq!(left.is_ok() as u8 + right.is_ok() as u8, 1);

        let delete = service_a.delete_canonical_realm_for_project("project-a", realm.id);
        let create = service_b.create_canonical_endpoint_for_project(
            "project-a",
            realm.id,
            "10.31.0.11".parse()?,
            "02:00:00:00:31:12".to_owned(),
        );
        let (delete_result, create_result) = tokio::join!(delete, create);
        if delete_result.is_ok() {
            assert!(create_result.is_err());
            assert!(
                service_a
                    .reconstruct_canonical_network("project-a", network.id)
                    .await?
                    .realms
                    .is_empty()
            );
        } else {
            assert!(create_result.is_ok());
            assert!(
                service_a
                    .reconstruct_canonical_network("project-a", network.id)
                    .await?
                    .realms
                    .iter()
                    .any(|value| value.id == realm.id)
            );
        }
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn independent_services_preserve_network_realm_races()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("canonical-network-realm-races");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store_a = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let network = service_a
            .create_canonical_network_for_project("project-a", "parent-race".to_owned())
            .await?;

        let (first, second) = tokio::join!(
            service_a.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.34.0.0/24".to_owned(),
                false,
            ),
            service_b.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.35.0.0/24".to_owned(),
                false,
            )
        );
        let created = [first, second]
            .into_iter()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        assert_eq!(created.len(), 2);
        let realms = service_a
            .reconstruct_canonical_network("project-a", network.id)
            .await?
            .realms;
        assert_eq!(realms.len(), 2);
        assert!(realms.iter().all(|realm| realm.network_id == network.id));

        let (delete, create) = tokio::join!(
            service_a.delete_canonical_network_for_project("project-a", network.id),
            service_b.create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.36.0.0/24".to_owned(),
                false,
            )
        );
        assert!(delete.is_err());
        assert!(create.is_ok());
        let snapshot = service_a
            .reconstruct_canonical_network("project-a", network.id)
            .await?;
        assert!(
            snapshot
                .realms
                .iter()
                .all(|realm| realm.network_id == snapshot.network.id)
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn realm_deletion_is_fenced_when_provider_binding_remains()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("realm-deletion-fence");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "fenced".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.30.0.0/24".to_owned(),
                false,
            )
            .await?;
        store
            .insert_canonical_realm_binding(&o3k_store::CanonicalRealmBindingRecord {
                fabric_domain_id: "fabric-a".to_owned(),
                realm_id: realm.id,
                provider_kind: "geneve".to_owned(),
                provider_segment_id: 300,
                binding_generation: 1,
                state: "active".to_owned(),
            })
            .await?;

        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        let deleting = store
            .get_canonical_realm("project-a", &realm.id)
            .await?
            .ok_or("realm disappeared during fenced deletion")?;
        assert_eq!(deleting.state, "deleting");
        assert!(matches!(
            service
                .create_canonical_endpoint_for_project(
                    "project-a",
                    realm.id,
                    "10.30.0.10".parse()?,
                    "02:00:00:30:00:10".to_owned(),
                )
                .await,
            Err(NetworkError::InvalidRequest) | Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(
            service
                .get_canonical_network_for_project("project-a", network.id)
                .await
                .is_ok()
        );
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn realm_cleanup_unknown_outcome_replays_and_finalizes_after_observation()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("realm-cleanup-recovery");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "recovery".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "10.31.0.0/24".to_owned(),
                false,
            )
            .await?;
        let binding = o3k_store::CanonicalRealmBindingRecord {
            fabric_domain_id: "fabric-a".to_owned(),
            realm_id: realm.id,
            provider_kind: "geneve".to_owned(),
            provider_segment_id: 301,
            binding_generation: 1,
            state: "active".to_owned(),
        };
        store.insert_canonical_realm_binding(&binding).await?;

        assert!(matches!(
            service
                .delete_canonical_realm_for_project("project-a", realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));
        let first = service
            .begin_canonical_realm_deletion_for_project("project-a", realm.id)
            .await?;
        let operation_id = match first {
            RealmCleanupProgress::AwaitingObservation { operation_id, .. } => operation_id,
            _ => return Err("unexpected replay progress".into()),
        };
        let unknown = service
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Unknown {
                    binding: binding.clone(),
                    reason: "provider response lost".to_owned(),
                }],
            )
            .await?;
        assert!(matches!(
            unknown,
            RealmCleanupProgress::AwaitingObservation { .. }
        ));
        assert_eq!(
            store.get_canonical_operation(operation_id).await?.state,
            o3k_store::OperationState::UnknownOutcome
        );

        drop(service);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let replay = reopened
            .begin_canonical_realm_deletion_for_project("project-a", realm.id)
            .await?;
        assert!(matches!(
            replay,
            RealmCleanupProgress::AwaitingObservation { operation_id: id, .. } if id == operation_id
        ));
        let present = reopened
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Present(binding.clone())],
            )
            .await?;
        assert!(matches!(
            present,
            RealmCleanupProgress::AwaitingObservation { .. }
        ));
        let removed = reopened
            .observe_canonical_realm_cleanup_for_project(
                "project-a",
                realm.id,
                vec![RealmCleanupObservation::Absent(binding)],
            )
            .await?;
        assert_eq!(removed, RealmCleanupProgress::Removed { operation_id });
        assert!(
            reopened
                .get_canonical_network_for_project("project-a", network.id)
                .await
                .is_ok()
        );
        assert!(matches!(
            reopened
                .reconstruct_canonical_network("project-a", network.id)
                .await,
            Ok(snapshot) if snapshot.realms.is_empty()
        ));
        let _ = fs::remove_dir_all(path);
        let _ = fs::remove_file(sqlite_path);
        Ok(())
    }

    #[tokio::test]
    async fn allocation_is_deterministic_collision_safe_and_restartable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("allocation");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let first = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        let second = service
            .create_port(&auth("project-a"), network.id, "two".to_owned())
            .await?;
        assert_ne!(first.fixed_ip, second.fixed_ip);
        assert_ne!(first.mac_address, second.mac_address);
        assert_eq!(first.mac_address, deterministic_port_mac(first.id));
        assert_eq!(first.fixed_ip, subnet.allocation_start);
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        assert_eq!(
            reopened.get_port(&auth("project-a"), first.id).await?,
            first
        );
        reopened.delete_port(&auth("project-a"), first.id).await?;
        let replacement = reopened
            .create_port(&auth("project-a"), network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, first.fixed_ip);
        assert!(!fs::read_dir(&path)?.flatten().any(|entry| {
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            name.contains("metadata.tmp-") || name.contains("metadata.json")
        }));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn legacy_metadata_file_is_imported_once_and_never_read_again()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("legacy-import");
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path)?;
        let network_id = Uuid::now_v7();
        let subnet_id = Uuid::now_v7();
        let port_with_mac = Uuid::now_v7();
        let port_without_mac = Uuid::now_v7();
        let port_without_subnet = Uuid::now_v7();
        let legacy = serde_json::json!({
            "networks": [{
                "id": network_id,
                "name": "flat",
                "project_id": "project-a",
                "status": "ACTIVE"
            }],
            "subnets": [{
                "id": subnet_id,
                "network_id": network_id,
                "name": "lab",
                "project_id": "project-a",
                "cidr": "192.0.2.0/29",
                "gateway_ip": "192.0.2.1",
                "allocation_start": "192.0.2.2",
                "allocation_end": "192.0.2.14"
            }],
            "ports": [
                {
                    "id": port_with_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "with-mac",
                    "mac_address": "02:00:00:00:00:99",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_mac,
                    "network_id": network_id,
                    "subnet_id": subnet_id,
                    "project_id": "project-a",
                    "name": "no-mac",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                },
                {
                    "id": port_without_subnet,
                    "network_id": network_id,
                    "project_id": "project-a",
                    "name": "no-subnet",
                    "mac_address": "02:00:00:00:00:98",
                    "fixed_ip": "192.0.2.4",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(path.join("metadata.json"), serde_json::to_vec(&legacy)?)?;
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        assert_eq!(service.list_networks(&auth("project-a")).await?.len(), 1);
        assert_eq!(service.list_subnets(&auth("project-a")).await?.len(), 1);
        assert_eq!(service.list_ports(&auth("project-a")).await?.len(), 3);
        let network = service.get_network(&auth("project-a"), network_id).await?;
        assert_eq!(network.id, network_id);
        let subnet = service.get_subnet(&auth("project-a"), subnet_id).await?;
        assert_eq!(subnet.id, subnet_id);
        let first = service.get_port(&auth("project-a"), port_with_mac).await?;
        assert_eq!(first.mac_address, "02:00:00:00:00:99");
        assert_eq!(first.subnet_id, Some(subnet_id));
        let migrated_mac = service
            .get_port(&auth("project-a"), port_without_mac)
            .await?;
        assert_eq!(
            migrated_mac.mac_address,
            deterministic_port_mac(port_without_mac)
        );
        assert_eq!(migrated_mac.subnet_id, Some(subnet_id));
        let migrated_subnet = service
            .get_port(&auth("project-a"), port_without_subnet)
            .await?;
        assert_eq!(migrated_subnet.subnet_id, Some(subnet_id));
        assert_eq!(migrated_subnet.mac_address, "02:00:00:00:00:98");
        assert!(!path.join("metadata.json").exists());
        assert!(path.join("metadata.json.imported").exists());
        let second = NetworkService::open(&path, store).await?;
        assert_eq!(second.list_networks(&auth("project-a")).await?.len(), 1);
        assert_eq!(second.list_subnets(&auth("project-a")).await?.len(), 1);
        assert_eq!(second.list_ports(&auth("project-a")).await?.len(), 3);
        drop(second);
        fs::remove_dir_all(path)?;

        let corrupt_path = root("legacy-import-corrupt");
        let _ = fs::remove_dir_all(&corrupt_path);
        fs::create_dir_all(&corrupt_path)?;
        fs::write(corrupt_path.join("metadata.json"), b"not-json")?;
        let corrupt_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&corrupt_path, corrupt_store).await,
            Err(NetworkError::CorruptMetadata(_))
        ));
        assert!(corrupt_path.join("metadata.json").exists());
        fs::remove_dir_all(corrupt_path)?;

        let duplicate_path = root("legacy-import-duplicate-mac");
        let _ = fs::remove_dir_all(&duplicate_path);
        fs::create_dir_all(&duplicate_path)?;
        let duplicated = serde_json::json!({
            "networks": [],
            "subnets": [],
            "ports": [
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "one",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.2",
                    "status": "ACTIVE"
                },
                {
                    "id": Uuid::now_v7(),
                    "network_id": Uuid::now_v7(),
                    "project_id": "project-a",
                    "name": "two",
                    "mac_address": "02:00:00:00:00:01",
                    "fixed_ip": "192.0.2.3",
                    "status": "ACTIVE"
                }
            ]
        });
        fs::write(
            duplicate_path.join("metadata.json"),
            serde_json::to_vec(&duplicated)?,
        )?;
        let duplicate_store = Arc::new(o3k_store::testkit::open_memory().await?);
        assert!(matches!(
            NetworkService::open(&duplicate_path, duplicate_store).await,
            Err(NetworkError::Conflict)
        ));
        assert!(duplicate_path.join("metadata.json").exists());
        fs::remove_dir_all(duplicate_path)?;
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_port_creation_never_allocates_duplicate_ips_or_macs()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-network-concurrent-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let setup_store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let setup = NetworkService::open(&path, setup_store.clone()).await?;
        let network = setup
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = setup
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/28".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(subnet.cidr, "192.0.2.0/28");
        drop(setup);
        drop(setup_store);

        let store_a = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let mut handles = Vec::new();
        for index in 0..12 {
            let service = if index % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            let network_id = network.id;
            handles.push(tokio::spawn(async move {
                service
                    .create_port(&auth("project-a"), network_id, format!("port-{index}"))
                    .await
            }));
        }
        let mut ports = Vec::new();
        for handle in handles {
            match handle.await? {
                Ok(port) => ports.push(port),
                Err(NetworkError::PoolExhausted) => {}
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(ports.len(), 12);
        let ips: HashSet<Ipv4Addr> = ports.iter().map(|port| port.fixed_ip).collect();
        let macs: HashSet<String> = ports
            .iter()
            .map(|port| port.mac_address.to_ascii_lowercase())
            .collect();
        assert_eq!(ports.len(), ips.len());
        assert_eq!(ports.len(), macs.len());
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_explicit_fixed_ip_creation_has_one_winner()
    -> Result<(), Box<dyn std::error::Error>> {
        let path =
            std::env::temp_dir().join(format!("o3k-network-explicit-race-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let setup_store = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let setup = NetworkService::open(&path, setup_store.clone()).await?;
        let network = setup
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = setup
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/28".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert!(matches!(
            setup
                .create_port_with_fixed_ip(
                    &auth("project-a"),
                    network.id,
                    "outside-pool".to_owned(),
                    Some((subnet.id, Some(Ipv4Addr::new(203, 0, 113, 5)))),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            setup
                .create_port_with_fixed_ip(
                    &auth("project-a"),
                    network.id,
                    "o3k-server:project-a:spoof".to_owned(),
                    Some((subnet.id, None)),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let server_port = setup
            .create_port_for_project(
                "project-a",
                network.id,
                "o3k-server:project-a:owned".to_owned(),
            )
            .await?;
        assert!(matches!(
            setup
                .update_port_name_for_project("project-a", server_port.id, "renamed".to_owned(),)
                .await,
            Err(NetworkError::Conflict)
        ));
        setup
            .delete_port_for_project("project-a", server_port.id)
            .await?;
        drop(setup);
        drop(setup_store);

        let service_a = NetworkService::open(
            &path,
            Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?),
        )
        .await?;
        let service_b = NetworkService::open(
            &path,
            Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?),
        )
        .await?;
        let fixed_ip = Ipv4Addr::new(192, 0, 2, 5);
        let first = tokio::spawn({
            let service = service_a.clone();
            async move {
                service
                    .create_port_with_fixed_ip(
                        &auth("project-a"),
                        network.id,
                        "first".to_owned(),
                        Some((subnet.id, Some(fixed_ip))),
                    )
                    .await
            }
        });
        let second = tokio::spawn({
            let service = service_b.clone();
            async move {
                service
                    .create_port_with_fixed_ip(
                        &auth("project-a"),
                        network.id,
                        "second".to_owned(),
                        Some((subnet.id, Some(fixed_ip))),
                    )
                    .await
            }
        });
        let outcomes = [first.await?, second.await?];
        assert_eq!(outcomes.iter().filter(|result| result.is_ok()).count(), 1);
        assert_eq!(
            outcomes
                .iter()
                .filter(|result| matches!(result, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn concurrent_cross_instance_writers_conflict_deterministically_without_duplicates()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = std::env::temp_dir().join(format!("o3k-network-multiwriter-{}", Uuid::now_v7()));
        let sqlite_path = path.with_extension("sqlite");
        fs::create_dir_all(&path)?;
        let store_a = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let store_b = Arc::new(o3k_store::testkit::open_file(&sqlite_path).await?);
        let service_a = NetworkService::open(&path, store_a).await?;
        let service_b = NetworkService::open(&path, store_b).await?;
        let auth_a = auth("project-a");
        let auth_b = auth("project-a");
        // Two writers create a network with the same name: exactly one wins.
        let (first, second) = tokio::join!(
            service_a.create_network(&auth_a, "flat".to_owned()),
            service_b.create_network(&auth_b, "flat".to_owned()),
        );
        assert_eq!([&first, &second].iter().filter(|r| r.is_ok()).count(), 1);
        assert_eq!(
            [&first, &second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        let network_id = first
            .or(second)
            .map_err(|_| "expected one network create to succeed")?
            .id;
        // Same cidr on the same network: exactly one subnet survives.
        let (subnet_first, subnet_second) = tokio::join!(
            service_a.create_subnet(
                &auth_a,
                network_id,
                "lab".to_owned(),
                "192.0.2.0/27".to_owned(),
                None,
                None,
                None,
            ),
            service_b.create_subnet(
                &auth_b,
                network_id,
                "lab".to_owned(),
                "192.0.2.0/27".to_owned(),
                None,
                None,
                None,
            ),
        );
        assert_eq!(
            [&subnet_first, &subnet_second]
                .iter()
                .filter(|r| r.is_ok())
                .count(),
            1
        );
        assert_eq!(
            [&subnet_first, &subnet_second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::Conflict)))
                .count(),
            1
        );
        // 40 concurrent port creates across two writers over a 29-address
        // pool: every allocation is distinct and the pool is exhausted
        // deterministically.
        let mut handles = Vec::new();
        for index in 0..40 {
            let service = if index % 2 == 0 {
                service_a.clone()
            } else {
                service_b.clone()
            };
            handles.push(tokio::spawn(async move {
                service
                    .create_port(&auth("project-a"), network_id, format!("port-{index}"))
                    .await
            }));
        }
        let mut ports = Vec::new();
        let mut exhausted = 0;
        for handle in handles {
            match handle.await? {
                Ok(port) => ports.push(port),
                Err(NetworkError::PoolExhausted) => exhausted += 1,
                Err(error) => return Err(error.into()),
            }
        }
        assert_eq!(ports.len(), 29);
        assert_eq!(exhausted, 11);
        let ips: HashSet<Ipv4Addr> = ports.iter().map(|port| port.fixed_ip).collect();
        let macs: HashSet<String> = ports
            .iter()
            .map(|port| port.mac_address.to_ascii_lowercase())
            .collect();
        assert_eq!(ports.len(), ips.len());
        assert_eq!(ports.len(), macs.len());
        // Concurrent deletion of one port: exactly one writer wins.
        let (delete_first, delete_second) = tokio::join!(
            service_a.delete_port(&auth_a, ports[0].id),
            service_b.delete_port(&auth_b, ports[0].id),
        );
        assert_eq!(
            [&delete_first, &delete_second]
                .iter()
                .filter(|r| r.is_ok())
                .count(),
            1
        );
        assert_eq!(
            [&delete_first, &delete_second]
                .iter()
                .filter(|r| matches!(r, Err(NetworkError::NotFound)))
                .count(),
            1
        );
        drop(service_a);
        drop(service_b);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{}-wal", sqlite_path.display()));
        let _ = fs::remove_file(format!("{}-shm", sqlite_path.display()));
        Ok(())
    }

    #[tokio::test]
    async fn binding_state_strings_round_trip_through_canonical_parsing() {
        for state in [
            PortBindingState::Binding,
            PortBindingState::Bound,
            PortBindingState::Down,
            PortBindingState::Error,
        ] {
            assert_eq!(PortBindingState::parse(state.as_str()), Some(state));
        }
        assert_eq!(PortBindingState::parse("unbound"), None);
        assert_eq!(PortBindingState::parse("banana"), None);
        assert_eq!(PortBindingState::parse(""), None);
    }

    #[tokio::test]
    async fn binding_intent_and_observation_projection_are_durable()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("binding");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let _subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        let intended = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(intended.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(intended.binding_state.as_deref(), Some("binding"));
        let observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(observed.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(observed.binding_state.as_deref(), Some("bound"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-1", "banana")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        // An idempotent dispatch replay of the same create must not downgrade
        // the completed `bound` observation back to `binding`.
        let replayed = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(replayed.binding_state.as_deref(), Some("bound"));
        // A fresh dispatch after an observed failure resets to `binding`.
        let down = service
            .project_binding_observation("project-a", port.id, "compute-1", "down")
            .await?;
        assert_eq!(down.binding_state.as_deref(), Some("down"));
        let retried = service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        assert_eq!(retried.binding_state.as_deref(), Some("binding"));
        assert!(matches!(
            service
                .project_binding_observation("project-a", port.id, "compute-2", "bound")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "compute-2")
                .await,
            Err(NetworkError::Conflict)
        ));
        assert!(matches!(
            service
                .project_binding_observation("project-a", Uuid::now_v7(), "compute-1", "bound")
                .await,
            Err(NetworkError::NotFound)
        ));
        assert!(matches!(
            service
                .record_binding_intent("project-a", port.id, "  ")
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let final_observed = service
            .project_binding_observation("project-a", port.id, "compute-1", "bound")
            .await?;
        assert_eq!(final_observed.binding_state.as_deref(), Some("bound"));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_port(&auth("project-a"), port.id).await?;
        assert_eq!(restored.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(restored.binding_state.as_deref(), Some("bound"));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn delete_cleanup_and_ip_reuse_after_restart() -> Result<(), Box<dyn std::error::Error>> {
        let path = root("delete-reuse");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        service.delete_port(&auth("project-a"), port.id).await?;
        assert!(matches!(
            service.get_port(&auth("project-a"), port.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let replacement = reopened
            .create_port(&auth("project-a"), network.id, "replacement".to_owned())
            .await?;
        assert_eq!(replacement.fixed_ip, port.fixed_ip);
        assert_ne!(replacement.mac_address, port.mac_address);
        reopened
            .delete_port(&auth("project-a"), replacement.id)
            .await?;
        reopened
            .delete_subnet(&auth("project-a"), subnet.id)
            .await?;
        reopened
            .delete_network(&auth("project-a"), network.id)
            .await?;
        assert!(matches!(
            reopened.get_network(&auth("project-a"), network.id).await,
            Err(NetworkError::NotFound)
        ));
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn create_outcome_projection_and_unbind_are_durable_and_idempotent()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("create-outcome");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        let _subnet = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "lab".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        // Without a recorded intent the projection is rejected.
        assert!(matches!(
            service
                .project_create_outcome("project-a", port.id, PortBindingState::Bound)
                .await,
            Err(NetworkError::Conflict)
        ));
        service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        // The observed state is set on the host recorded by the intent.
        let bound = service
            .project_create_outcome("project-a", port.id, PortBindingState::Bound)
            .await?;
        assert_eq!(bound.binding_host.as_deref(), Some("compute-1"));
        assert_eq!(bound.binding_state.as_deref(), Some("bound"));
        // A failed outcome after a fresh intent projects `error`.
        service
            .project_binding_observation("project-a", port.id, "compute-1", "down")
            .await?;
        service
            .record_binding_intent("project-a", port.id, "compute-1")
            .await?;
        let errored = service
            .project_create_outcome("project-a", port.id, PortBindingState::Error)
            .await?;
        assert_eq!(errored.binding_state.as_deref(), Some("error"));
        // Only terminal create outcomes are projectable.
        assert!(matches!(
            service
                .project_create_outcome("project-a", port.id, PortBindingState::Binding)
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            service
                .project_create_outcome("project-a", Uuid::now_v7(), PortBindingState::Bound)
                .await,
            Err(NetworkError::NotFound)
        ));
        // Unbind clears the binding idempotently and is durable.
        let unbound = service.unbind_port("project-a", port.id).await?;
        assert_eq!(unbound.binding_host, None);
        assert_eq!(unbound.binding_state, None);
        let again = service.unbind_port("project-a", port.id).await?;
        assert_eq!(again.binding_host, None);
        assert!(matches!(
            service.unbind_port("project-a", Uuid::now_v7()).await,
            Err(NetworkError::NotFound)
        ));
        drop(service);
        drop(store);
        let reopened_store =
            Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let reopened = NetworkService::open(&path, reopened_store.clone()).await?;
        let restored = reopened.get_port(&auth("project-a"), port.id).await?;
        assert_eq!(restored.binding_host, None);
        assert_eq!(restored.binding_state, None);
        drop(reopened);
        drop(reopened_store);
        fs::remove_dir_all(path)?;
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn invalid_cidr_exhaustion_and_project_isolation_are_enforced()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("validation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store).await?;
        let network = service
            .create_network(&auth("project-a"), "flat".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_subnet(
                    &auth("project-a"),
                    network.id,
                    "bad".to_owned(),
                    "192.0.2.1/31".to_owned(),
                    None,
                    None,
                    None
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        let _ = service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "tiny".to_owned(),
                "192.0.2.0/30".to_owned(),
                None,
                Some(Ipv4Addr::new(192, 0, 2, 2)),
                Some(Ipv4Addr::new(192, 0, 2, 2)),
            )
            .await?;
        let _ = service
            .create_port(&auth("project-a"), network.id, "one".to_owned())
            .await?;
        assert!(matches!(
            service
                .create_port(&auth("project-a"), network.id, "two".to_owned())
                .await,
            Err(NetworkError::PoolExhausted)
        ));
        assert!(matches!(
            service
                .create_subnet(
                    &auth("project-a"),
                    network.id,
                    "gateway-overlap".to_owned(),
                    "198.51.100.0/29".to_owned(),
                    Some(Ipv4Addr::new(198, 51, 100, 3)),
                    Some(Ipv4Addr::new(198, 51, 100, 2)),
                    Some(Ipv4Addr::new(198, 51, 100, 4)),
                )
                .await,
            Err(NetworkError::InvalidRequest)
        ));
        assert!(matches!(
            service.get_network(&auth("project-b"), network.id).await,
            Err(NetworkError::NotFound)
        ));
        fs::remove_dir_all(path)?;
        Ok(())
    }

    #[tokio::test]
    async fn network_quota_enforcement_and_isolation() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::QuotaRepository;

        let path = root("network-quota-isolation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-a"), None, None);

        // Limit proj-a to 1 network
        store
            .set_limit(
                &scope_a,
                &LimitKey::network_networks(),
                LimitValue::Maximum(1),
            )
            .await?;

        let auth_a = auth("proj-a");
        let auth_b = auth("proj-b");

        // 1. First network for proj-a succeeds
        let net1 = service.create_network(&auth_a, "net-1".to_owned()).await?;
        assert_eq!(net1.name, "net-1");

        // 2. Second network for proj-a fails with QuotaExceeded
        let res2 = service.create_network(&auth_a, "net-2".to_owned()).await;
        assert!(matches!(res2, Err(NetworkError::QuotaExceeded { .. })));

        // 3. Proj-b can create network (isolation)
        let net_b = service.create_network(&auth_b, "net-b".to_owned()).await?;
        assert_eq!(net_b.name, "net-b");

        // 4. Deleting net1 frees quota for proj-a
        service.delete_network(&auth_a, net1.id).await?;

        let net2 = service.create_network(&auth_a, "net-2".to_owned()).await?;
        assert_eq!(net2.name, "net-2");

        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[tokio::test]
    async fn network_subnet_and_port_quota_enforcement() -> Result<(), Box<dyn std::error::Error>> {
        use o3k_store::QuotaRepository;

        let path = root("network-subnets-ports-quota");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;

        let scope_a = OwnershipScope::project(ScopeId::new_unchecked("proj-sub-port"), None, None);
        let auth_a = auth("proj-sub-port");

        // 1. Set subnet limit = 1 and port limit = 1
        store
            .set_limit(
                &scope_a,
                &LimitKey::network_subnets(),
                LimitValue::Maximum(1),
            )
            .await?;
        store
            .set_limit(&scope_a, &LimitKey::network_ports(), LimitValue::Maximum(1))
            .await?;

        let net = service
            .create_network(&auth_a, "net-main".to_owned())
            .await?;

        // 2. Subnet creation: 1st succeeds, 2nd fails
        let sub1 = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-1".to_owned(),
                "10.0.0.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(sub1.network_id, net.id);

        let sub2_res = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-2".to_owned(),
                "10.0.1.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await;
        assert!(matches!(sub2_res, Err(NetworkError::QuotaExceeded { .. })));

        // 3. Port creation: 1st succeeds, 2nd fails
        let port1 = service
            .create_port(&auth_a, net.id, "port-1".to_owned())
            .await?;
        assert_eq!(port1.network_id, net.id);

        let port2_res = service
            .create_port(&auth_a, net.id, "port-2".to_owned())
            .await;
        assert!(matches!(port2_res, Err(NetworkError::QuotaExceeded { .. })));

        // 4. Delete port1 and subnet1 -> frees quota
        service.delete_port(&auth_a, port1.id).await?;
        service.delete_subnet(&auth_a, sub1.id).await?;

        // 5. Subsequent creates succeed
        let sub2 = service
            .create_subnet(
                &auth_a,
                net.id,
                "sub-2".to_owned(),
                "10.0.1.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        assert_eq!(sub2.network_id, net.id);

        let port2 = service
            .create_port(&auth_a, net.id, "port-2".to_owned())
            .await?;
        assert_eq!(port2.network_id, net.id);

        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[tokio::test]
    async fn policy_intent_is_durable_generation_fenced_and_compiled()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("policy-intent");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let project = "project-a";
        let network = service
            .create_network(&auth(project), "policy-net".to_owned())
            .await?;
        service
            .create_subnet(
                &auth(project),
                network.id,
                "policy-subnet".to_owned(),
                "10.20.0.0/24".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth(project), network.id, "policy-port".to_owned())
            .await?;
        let policy_id = Uuid::now_v7();
        let policy = PolicyIntent {
            id: policy_id,
            endpoint_id: port.id,
            direction: PolicyDirection::Ingress,
            protocol: NetworkProtocol::Tcp,
            ports: Some(o3k_domain::PortRange {
                start: 8080,
                end: 8080,
            }),
            source: Some(
                o3k_domain::Ipv4Prefix::new("198.51.100.0".parse()?, 24)
                    .ok_or("invalid source prefix")?,
            ),
            destination: None,
            action: PolicyAction::Deny,
        };
        service
            .upsert_policy_for_project(project, network.id, policy.clone())
            .await?;
        assert_eq!(
            service
                .list_policies_for_project(project, network.id)
                .await?,
            vec![policy.clone()]
        );
        let canonical = store.list_canonical_policies(project, &network.id).await?;
        assert_eq!(canonical.len(), 1);
        assert_eq!(canonical[0].id, policy_id);
        let legacy = store.get_network_intent(project, &network.id).await?;
        assert!(!legacy.is_some_and(|record| record.payload.contains(&policy_id.to_string())));

        let compiled = compile_attachment_plan(AttachmentPlanInput {
            endpoint_id: port.id,
            realm_id: network.id,
            project_id: project,
            mac: &port.mac_address,
            fixed_ip: port.fixed_ip,
            subnet_cidr: "10.20.0.0/24",
            node_id: "network-agent-1",
            operation_id: Uuid::now_v7(),
            deadline_unix_ms: 1,
            public_address: None,
            external_realm_id: None,
            policies: vec![policy.clone()],
        })?;
        assert!(compiled.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::Policy(value) if value == &policy
        )));

        service
            .delete_policy_for_project(project, network.id, policy_id)
            .await?;
        assert!(
            service
                .list_policies_for_project(project, network.id)
                .await?
                .is_empty()
        );
        assert!(matches!(
            service
                .delete_policy_for_project("other-project", network.id, policy_id)
                .await,
            Err(NetworkError::NotFound)
        ));
        let _ = fs::remove_dir_all(&path);
        Ok(())
    }

    #[test]
    fn attachment_plan_can_carry_operator_owned_routed_egress() -> Result<(), NetworkPlanError> {
        let endpoint_id = Uuid::from_u128(11);
        let external_realm_id = Uuid::from_u128(12);
        let plan = compile_attachment_plan(AttachmentPlanInput {
            endpoint_id,
            realm_id: Uuid::from_u128(13),
            project_id: "project-a",
            mac: "02:00:00:00:00:0b",
            fixed_ip: Ipv4Addr::new(10, 0, 0, 2),
            subnet_cidr: "10.0.0.0/24",
            node_id: "node-a",
            operation_id: Uuid::from_u128(14),
            deadline_unix_ms: 1,
            public_address: None,
            external_realm_id: Some(external_realm_id),
            policies: Vec::new(),
        })?;
        assert!(plan.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::Egress(o3k_domain::EgressIntent {
                external_realm_id: id,
                enabled: true,
                nat: true,
            }) if *id == external_realm_id
        )));
        assert!(plan
            .intents
            .iter()
            .any(|intent| matches!(intent, NetworkPlanIntent::EndpointAttachment { endpoint_id: id, .. } if *id == endpoint_id)));
        Ok(())
    }

    #[tokio::test]
    async fn security_group_rules_project_to_endpoint_policy_and_enforce_scope()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("security-groups");
        let sqlite_path = format!("{}.sqlite", path.display());
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let store = Arc::new(o3k_store::testkit::open_file(Path::new(&sqlite_path)).await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_network(&auth("project-a"), "net".to_owned())
            .await?;
        service
            .create_subnet(
                &auth("project-a"),
                network.id,
                "subnet".to_owned(),
                "192.0.2.0/29".to_owned(),
                None,
                None,
                None,
            )
            .await?;
        let port = service
            .create_port(&auth("project-a"), network.id, "port".to_owned())
            .await?;
        let group = service
            .create_security_group_for_project("project-a", "web".to_owned(), String::new())
            .await?;
        let second_group = service
            .create_security_group_for_project("project-a", "api".to_owned(), String::new())
            .await?;
        let rule = service
            .create_security_group_rule_for_project(
                "project-a",
                group.id,
                "ingress".to_owned(),
                "tcp".to_owned(),
                Some(443),
                Some(443),
                Some("0.0.0.0/0".to_owned()),
            )
            .await?;
        let first_change = service
            .replace_security_group_bindings_for_project("project-a", port.id, vec![group.id])
            .await?;
        assert!(first_change.is_empty());
        let first_attachment = store
            .list_endpoint_policy_attachments("project-a", &port.id)
            .await?
            .into_iter()
            .find(|attachment| attachment.policy_id == group.id)
            .ok_or("initial attachment missing")?;
        let unchanged = service
            .replace_security_group_bindings_for_project("project-a", port.id, vec![group.id])
            .await?;
        assert!(unchanged.is_empty());
        let unchanged_attachment = store
            .list_endpoint_policy_attachments("project-a", &port.id)
            .await?
            .into_iter()
            .find(|attachment| attachment.policy_id == group.id)
            .ok_or("unchanged attachment missing")?;
        assert_eq!(unchanged_attachment.id, first_attachment.id);
        assert_eq!(unchanged_attachment.generation, first_attachment.generation);
        let added = service
            .replace_security_group_bindings_for_project(
                "project-a",
                port.id,
                vec![group.id, second_group.id],
            )
            .await?;
        assert!(added.is_empty());
        let attachments = store
            .list_endpoint_policy_attachments("project-a", &port.id)
            .await?;
        assert_eq!(attachments.len(), 2);
        assert_eq!(
            attachments
                .iter()
                .find(|attachment| attachment.policy_id == group.id)
                .map(|attachment| attachment.id),
            Some(first_attachment.id)
        );
        let updated_group = service
            .update_security_group_for_project(
                "project-a",
                group.id,
                "web-renamed".to_owned(),
                "updated".to_owned(),
            )
            .await?;
        assert_eq!(updated_group.name, "web-renamed");
        let canonical_group = store
            .get_reusable_policy("project-a", &group.id)
            .await?
            .ok_or("canonical security group missing")?;
        assert_eq!(canonical_group.generation, 2);
        assert_eq!(canonical_group.stateful_mode, "Stateful");
        assert_eq!(canonical_group.unmatched_action, "Deny");
        let defaults = service
            .policy_defaults_for_endpoint("project-a", port.id)
            .await?;
        assert_eq!(defaults.len(), 2);
        assert!(
            defaults
                .iter()
                .all(|default| default.endpoint_id == port.id)
        );
        assert!(
            defaults
                .iter()
                .all(|default| default.unmatched_action == PolicyAction::Deny)
        );
        let default_plan = compile_attachment_plan_with_defaults(
            AttachmentPlanInput {
                endpoint_id: port.id,
                realm_id: network.id,
                project_id: "project-a",
                mac: &port.mac_address,
                fixed_ip: port.fixed_ip,
                subnet_cidr: "192.0.2.0/29",
                node_id: "network-agent-1",
                operation_id: Uuid::now_v7(),
                deadline_unix_ms: 1,
                public_address: None,
                external_realm_id: None,
                policies: Vec::new(),
            },
            defaults,
        )?;
        assert!(default_plan.intents.iter().any(|intent| matches!(
            intent,
            NetworkPlanIntent::PolicyDefault(default)
                if default.policy_id == group.id
                    && default.unmatched_action == PolicyAction::Deny
        )));
        let canonical_rules = store.list_policy_rules("project-a", &group.id).await?;
        assert_eq!(canonical_rules.len(), 1);
        assert_eq!(canonical_rules[0].id, rule.id);
        let canonical_attachments = store
            .list_endpoint_policy_attachments("project-a", &port.id)
            .await?;
        assert_eq!(canonical_attachments.len(), 2);
        assert!(
            canonical_attachments
                .iter()
                .any(|attachment| attachment.policy_id == group.id)
        );
        assert!(
            canonical_attachments
                .iter()
                .all(|attachment| attachment.id != attachment.policy_id)
        );
        let policies = service
            .list_policies_for_project("project-a", network.id)
            .await?;
        assert!(policies.iter().any(|policy| policy.id == rule.id
            && policy.endpoint_id == port.id
            && policy.action == PolicyAction::Allow));
        assert!(
            service
                .list_policies_for_project("project-b", network.id)
                .await
                .is_err()
        );
        let _ = fs::remove_dir_all(&path);
        let _ = fs::remove_file(&sqlite_path);
        let _ = fs::remove_file(format!("{sqlite_path}-wal"));
        let _ = fs::remove_file(format!("{sqlite_path}-shm"));
        Ok(())
    }

    #[tokio::test]
    async fn gateway_delete_reservation_reconstructs_a_generation_fenced_snapshot()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("gateway-delete-reservation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let gateway = service
            .create_l3_gateway_for_project("project-a", "edge".to_owned(), None, true)
            .await?;

        let deleting = service
            .delete_l3_gateway_for_project("project-a", &gateway.id, gateway.generation)
            .await?;
        assert_eq!(deleting.state, "deleting");
        assert_eq!(deleting.generation, gateway.generation + 1);
        assert_eq!(service.list_deleting_l3_gateways().await?.len(), 1);

        // A retry/restart can rebuild the exact removal target from the
        // durable reservation; it must not need the pre-delete row in memory.
        let snapshot = service
            .compile_l3_gateway_execution_plan_for_project("project-a", &gateway.id)
            .await?;
        assert_eq!(snapshot.gateway_id, gateway.id);
        assert_eq!(snapshot.gateway_generation, deleting.generation);
        assert!(snapshot.attachments.is_empty());
        assert_eq!(
            store
                .get_canonical_l3_gateway("project-a", &gateway.id)
                .await?
                .ok_or("gateway reservation disappeared")?
                .state,
            "deleting"
        );
        Ok(())
    }

    #[tokio::test]
    async fn attachment_detach_reservation_is_gateway_scoped_and_not_finalized_implicitly()
    -> Result<(), Box<dyn std::error::Error>> {
        let path = root("gateway-detach-reservation");
        let _ = fs::remove_dir_all(&path);
        let store = Arc::new(o3k_store::testkit::open_memory().await?);
        let service = NetworkService::open(&path, store.clone()).await?;
        let network = service
            .create_canonical_network_for_project("project-a", "net".to_owned())
            .await?;
        let realm = service
            .create_canonical_realm_for_project(
                "project-a",
                network.id,
                "192.0.2.0/24".to_owned(),
                false,
            )
            .await?;
        let gateway = service
            .create_l3_gateway_for_project("project-a", "edge".to_owned(), None, true)
            .await?;
        let attachment = service
            .attach_l3_gateway_realm("project-a", &gateway.id, &realm.id)
            .await?;
        let deleting = service
            .detach_l3_gateway_realm("project-a", &attachment.id, attachment.generation)
            .await?;

        assert_eq!(deleting.state, "deleting");
        assert_eq!(deleting.generation, attachment.generation + 1);
        assert_eq!(
            service.list_deleting_l3_gateway_attachments().await?.len(),
            1
        );
        assert!(matches!(
            service
                .attach_l3_gateway_realm("project-a", &gateway.id, &realm.id)
                .await,
            Err(NetworkError::Conflict)
        ));

        // The relation remains present until an external provider observation
        // authorizes finalization, while the gateway snapshot excludes it.
        let snapshot = service
            .compile_l3_gateway_execution_plan_for_project("project-a", &gateway.id)
            .await?;
        assert_eq!(snapshot.gateway_id, gateway.id);
        assert!(snapshot.attachments.is_empty());
        let persisted = store
            .get_canonical_l3_gateway_attachment("project-a", &attachment.id)
            .await?
            .ok_or("attachment reservation disappeared")?;
        assert_eq!(persisted.state, "deleting");
        assert_eq!(persisted.generation, deleting.generation);

        service
            .finalize_l3_gateway_realm_detachment_for_project(
                "project-a",
                &attachment.id,
                deleting.generation,
            )
            .await?;
        assert!(
            store
                .get_canonical_l3_gateway_attachment("project-a", &attachment.id)
                .await?
                .is_none()
        );
        Ok(())
    }
}
