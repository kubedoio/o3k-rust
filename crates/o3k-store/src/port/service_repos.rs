use async_trait::async_trait;
use uuid::Uuid;

use crate::domain::error::StoreError;
use crate::domain::records::{
    AuditEventRecord, CanonicalAddressPoolRecord, CanonicalAddressRealmRecord,
    CanonicalEndpointRecord, CanonicalL3GatewayAttachmentRecord, CanonicalL3GatewayRecord,
    CanonicalNetworkPolicyRecord, CanonicalNetworkRecord, CanonicalRealmBindingRecord,
    FederatedBindingRecord, ImageMetadataRecord, KeypairRecord, KeystoneDomainRecord,
    KeystoneEndpointRecord, KeystoneProjectRecord, KeystoneRegionRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord, KeystoneUserRecord,
    MeteringAggregate, MeteringEventRecord, NetworkAddressAllocationRecord, NetworkIntentRecord,
    NetworkRecord, OperatorAssignmentRecord, PlacementAllocationRecord, PlacementCapacityRecord,
    PlacementIntentRecord, PlacementInventoryRecord, PlacementProviderRecord,
    PlacementReconcileRecord, PortRecord, PublicAddressBindingRecord, ResourceRecord,
    SecurityGroupBindingRecord, SecurityGroupRecord, SecurityGroupRuleRecord, SubnetRecord,
    VolumeAttachmentRecord,
};
use crate::port::durable::DurableStore;
use crate::quota::QuotaRepository;

/// Durable metering authority. Events are append-only, idempotent by event ID,
/// and aggregate queries are bounded and scoped in SQL.
#[async_trait]
pub trait MeteringRepository: Send + Sync {
    async fn append_metering_event(&self, event: &MeteringEventRecord) -> Result<(), StoreError>;
    async fn aggregate_metering_events(
        &self,
        project_id: &str,
        meter_id: &str,
        effective_from: &str,
        effective_to: &str,
        limit: usize,
    ) -> Result<MeteringAggregate, StoreError>;
}

/// Durable audit authority. Implementations enforce scope in SQL and return
/// at most `limit` rows; callers must not use this as an unbounded history API.
#[async_trait]
pub trait AuditRepository: Send + Sync {
    async fn append_audit_event(&self, event: &AuditEventRecord) -> Result<(), StoreError>;
    /// Deletes at most `limit` events older than the supplied canonical
    /// timestamp. Retention is deliberately a bounded maintenance primitive;
    /// callers must repeat it until zero rows are returned.
    async fn prune_audit_events_before(
        &self,
        timestamp: &str,
        limit: usize,
    ) -> Result<usize, StoreError>;
    async fn list_audit_events_page(
        &self,
        effective_scope: &str,
        after_event_id: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError>;
    async fn list_audit_events_system_page(
        &self,
        scope: Option<&str>,
        after_event_id: Option<&str>,
        limit: usize,
        filters: &AuditEventFilters,
    ) -> Result<Vec<AuditEventRecord>, StoreError>;
    async fn get_audit_event(
        &self,
        effective_scope: &str,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError>;
    async fn get_audit_event_system(
        &self,
        event_id: &str,
    ) -> Result<Option<AuditEventRecord>, StoreError>;
}

/// Indexed public predicates for bounded audit investigation. Effective scope
/// is supplied separately by the authorization boundary and cannot be chosen
/// by a tenant query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AuditEventFilters {
    pub event_id: Option<String>,
    pub timestamp_from: Option<String>,
    pub timestamp_to: Option<String>,
    pub principal_id: Option<String>,
    pub service_namespace: Option<String>,
    pub action: Option<String>,
    pub outcome: Option<String>,
    pub resource_type: Option<String>,
    pub resource_id: Option<String>,
    pub operation_id: Option<String>,
    pub request_id: Option<String>,
    pub audit_id: Option<String>,
}

/// Bounded governance reads over canonical IAM records. This is separate
/// from historical identity bootstrap snapshot APIs.
#[async_trait]
pub trait GovernanceRepository: Send + Sync {
    /// Atomically accepts a role assignment, its canonical operation and
    /// durable audit admission. Existing idempotency keys are replayed only
    /// when their fingerprint and assignment identity match.
    async fn mutate_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
        audit: &AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, StoreError>;
    async fn mutate_role_assignment_removal(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
        audit: &AuditEventRecord,
    ) -> Result<crate::IdempotencyReservation, StoreError>;
    /// Atomically establishes a project role assignment and returns the
    /// canonical durable row.  Repeating the same principal/project/role
    /// tuple converges on the existing row; callers must use the returned ID
    /// rather than treating the request ID as authority.
    async fn ensure_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<KeystoneRoleAssignmentRecord, StoreError>;
    async fn get_role_assignment(
        &self,
        id: &str,
    ) -> Result<Option<KeystoneRoleAssignmentRecord>, StoreError>;
    /// Removes one canonical assignment. The tuple is required so a caller
    /// cannot delete an unrelated assignment by guessing an opaque row id.
    async fn remove_role_assignment(
        &self,
        principal_id: &str,
        project_id: &str,
        role_id: &str,
    ) -> Result<bool, StoreError>;
    async fn get_project(&self, id: &str) -> Result<Option<KeystoneProjectRecord>, StoreError>;
    async fn get_principal(&self, id: &str) -> Result<Option<KeystoneUserRecord>, StoreError>;
    async fn list_projects_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneProjectRecord>, StoreError>;
    async fn list_principals_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneUserRecord>, StoreError>;
    async fn list_role_assignments_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError>;
    /// Returns the canonical role catalog in stable ID order. This is a
    /// bounded page so native governance cannot turn IAM cardinality into an
    /// unbounded in-memory collection.
    async fn list_roles_page(
        &self,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<KeystoneRoleRecord>, StoreError>;
}

/// Durable Keystone-compatible identity records used by the identity
/// application service: deterministic bootstrap seeding (upserts) and the
/// one-time snapshot load that feeds token issuance and the catalog.
///
/// This is a narrow port around the identity use cases, not a generic
/// persistence surface. Application code depends on this trait (or a broader
/// combined port) instead of on the concrete `SqliteStore` adapter.
#[async_trait]
pub trait IdentityRepository: Send + Sync {
    async fn insert_keystone_domain(&self, domain: &KeystoneDomainRecord)
    -> Result<(), StoreError>;
    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError>;
    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError>;
    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError>;
    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError>;
    async fn insert_federated_binding(
        &self,
        binding: &FederatedBindingRecord,
    ) -> Result<(), StoreError>;
    async fn find_federated_binding(
        &self,
        trusted_issuer_id: &str,
        subject: &str,
    ) -> Result<Option<FederatedBindingRecord>, StoreError>;
    async fn list_federated_bindings(&self) -> Result<Vec<FederatedBindingRecord>, StoreError>;
    async fn set_federated_binding_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<(), StoreError>;
    async fn insert_operator_assignment(
        &self,
        assignment: &OperatorAssignmentRecord,
    ) -> Result<(), StoreError>;
    async fn list_operator_assignments(&self) -> Result<Vec<OperatorAssignmentRecord>, StoreError>;
    async fn set_operator_assignment_enabled(
        &self,
        id: &str,
        enabled: bool,
    ) -> Result<(), StoreError>;
    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError>;
    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError>;
    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError>;
    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError>;
    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError>;
    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError>;
    async fn insert_keystone_region(&self, region: &KeystoneRegionRecord)
    -> Result<(), StoreError>;
    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError>;
}

/// Durable keypair records owned by the compute service. The trait keeps the
/// scoped uniqueness, attach, and delete semantics available to application
/// code without naming the concrete adapter.
#[async_trait]
pub trait KeypairRepository: Send + Sync {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError>;
    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError>;
    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError>;
    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError>;
    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError>;
    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError>;
    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError>;
}

/// Durable Nova volume-attachment records owned by the compute attachment
/// orchestrator. Phase and outcome updates carry the frozen Cinder attachment
/// lifecycle; the port exposes the exact transitions the orchestrator uses.
#[async_trait]
pub trait VolumeAttachmentRepository: Send + Sync {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError>;
    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError>;
    #[allow(clippy::too_many_arguments)]
    async fn update_volume_attachment_outcome(
        &self,
        id: Uuid,
        status: &str,
        cinder_attachment_id: Option<&str>,
        connector_host: Option<&str>,
        connector_ip: Option<&str>,
        connector_initiator: Option<&str>,
        driver_volume_type: Option<&str>,
        target_iqn: Option<&str>,
        target_portal: Option<&str>,
        target_lun: Option<u32>,
        connection_info_digest: Option<&str>,
        device: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError>;
    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError>;
    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError>;
    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError>;
    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError>;
}

/// Durable canonical public-address authority. Provider realization must use
/// this repository as its source of tenant-visible allocation state.
#[async_trait]
pub trait PublicAddressRepository: Send + Sync {
    async fn allocate_public_address(
        &self,
        project_id: &str,
        operation_id: &str,
        allocation_id: Uuid,
        first_usable: std::net::Ipv4Addr,
        last_usable: std::net::Ipv4Addr,
    ) -> Result<PublicAddressBindingRecord, StoreError>;
    async fn associate_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
        endpoint_id: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError>;
    async fn disassociate_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<PublicAddressBindingRecord, StoreError>;
    async fn release_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<(), StoreError>;
    async fn get_public_address(
        &self,
        project_id: &str,
        allocation_id: Uuid,
    ) -> Result<Option<PublicAddressBindingRecord>, StoreError>;
    async fn list_public_addresses(
        &self,
        project_id: &str,
        limit: usize,
    ) -> Result<Vec<PublicAddressBindingRecord>, StoreError>;
}

/// Durable Glance-compatible image metadata owned by the image service:
/// project ownership, format/visibility, and the size/checksum sealed by the
/// queued -> active transition. The bounded artifact bytes stay in the
/// filesystem content directory; this port owns only the metadata.
///
/// This is a narrow port around the image use cases, not a generic
/// persistence surface. Application code depends on this trait instead of on
/// the concrete `SqliteStore` adapter.
#[async_trait]
pub trait ImageRepository: Send + Sync + DurableStore + QuotaRepository {
    /// Atomically admits (or replays) an image mutation in the canonical
    /// operation/idempotency tables.  Image metadata remains authoritative in
    /// this service-specific repository; callers must perform this admission
    /// before writing artifact bytes or changing image state.
    async fn create_or_replay_canonical_image_operation(
        &self,
        operation: &crate::OperationRecord,
        canonical: &crate::CanonicalOperationRecord,
        request: &crate::IdempotencyReservationRequest,
    ) -> Result<crate::IdempotencyReservation, StoreError>;
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError>;
    /// Atomically persists image metadata and its secret-safe mutation audit.
    /// The audit row is part of the same database transaction as the image
    /// insert, so a successful image mutation cannot become unaudited.
    async fn insert_image_with_audit(
        &self,
        image: &ImageMetadataRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError>;
    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError>;
    async fn list_images_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ImageMetadataRecord>, StoreError>;
    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError>;
    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError>;
    /// Atomically seals an image and records its mutation audit.
    async fn activate_image_with_audit(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
        audit: &AuditEventRecord,
    ) -> Result<ImageMetadataRecord, StoreError>;
    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;
    /// Atomically tombstones an image and records its mutation audit.
    async fn delete_image_with_audit(
        &self,
        project_id: &str,
        id: &Uuid,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError>;
}

/// Durable Neutron-compatible network/subnet/port metadata owned by the
/// network service: project ownership, addressing and allocation ranges, and
/// port binding state. This port owns only the metadata; network datapath
/// behavior stays outside the store.
///
/// This is a narrow port around the network use cases, not a generic
/// persistence surface. Application code depends on this trait instead of on
/// the concrete `SqliteStore` adapter.
#[async_trait]
pub trait NetworkRepository:
    Send + Sync + DurableStore + QuotaRepository + crate::CanonicalPolicyRepository
{
    /// Resolves canonical ownership for authorization without exposing an
    /// unscoped public read API. The network service uses this only to build
    /// an authorization target before applying project non-disclosure.
    async fn get_canonical_owner(
        &self,
        resource_name: &str,
        id: &Uuid,
    ) -> Result<Option<String>, StoreError>;
    async fn insert_canonical_network(
        &self,
        network: &CanonicalNetworkRecord,
    ) -> Result<(), StoreError>;
    /// Atomically commits canonical network authority and its successful
    /// mutation audit projection in one database transaction.
    async fn insert_canonical_network_with_audit(
        &self,
        network: &CanonicalNetworkRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError>;
    async fn get_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalNetworkRecord>, StoreError>;
    async fn list_canonical_networks(
        &self,
        project_id: &str,
    ) -> Result<Vec<CanonicalNetworkRecord>, StoreError>;
    async fn list_canonical_networks_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CanonicalNetworkRecord>, StoreError>;
    async fn update_canonical_network(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        name: &str,
        admin_state_up: bool,
    ) -> Result<CanonicalNetworkRecord, StoreError>;
    async fn insert_canonical_l3_gateway(
        &self,
        gateway: &CanonicalL3GatewayRecord,
    ) -> Result<(), StoreError>;
    async fn get_canonical_l3_gateway(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayRecord>, StoreError>;
    async fn list_canonical_l3_gateways(
        &self,
        project_id: &str,
    ) -> Result<Vec<CanonicalL3GatewayRecord>, StoreError>;
    /// Transitional-resource inventory used by startup recovery.
    async fn list_canonical_l3_gateways_by_state(
        &self,
        state: &str,
    ) -> Result<Vec<CanonicalL3GatewayRecord>, StoreError>;
    async fn update_canonical_l3_gateway(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        name: &str,
        external_realm_id: Option<Uuid>,
        enable_snat: bool,
    ) -> Result<CanonicalL3GatewayRecord, StoreError>;
    async fn begin_canonical_l3_gateway_deletion(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
    ) -> Result<CanonicalL3GatewayRecord, StoreError>;
    async fn finalize_canonical_l3_gateway_deletion(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), StoreError>;
    async fn insert_canonical_l3_gateway_attachment(
        &self,
        attachment: &CanonicalL3GatewayAttachmentRecord,
    ) -> Result<(), StoreError>;
    async fn get_canonical_l3_gateway_attachment(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalL3GatewayAttachmentRecord>, StoreError>;
    async fn list_canonical_l3_gateway_attachments(
        &self,
        project_id: &str,
        gateway_id: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError>;
    async fn list_canonical_l3_gateway_attachments_by_state(
        &self,
        state: &str,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError>;
    async fn list_canonical_realm_l3_gateway_attachments(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalL3GatewayAttachmentRecord>, StoreError>;
    async fn begin_canonical_l3_gateway_attachment_deletion(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
    ) -> Result<CanonicalL3GatewayAttachmentRecord, StoreError>;
    async fn finalize_canonical_l3_gateway_attachment_deletion(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), StoreError>;
    async fn insert_canonical_realm(
        &self,
        realm: &CanonicalAddressRealmRecord,
    ) -> Result<(), StoreError>;
    /// Atomically commits an address-realm mutation and its successful audit.
    async fn insert_canonical_realm_with_audit(
        &self,
        realm: &CanonicalAddressRealmRecord,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError>;
    async fn get_canonical_realm(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<CanonicalAddressRealmRecord>, StoreError>;
    async fn list_canonical_realms(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressRealmRecord>, StoreError>;
    async fn list_canonical_realms_page(
        &self,
        project_id: &str,
        after_id: Option<&str>,
        limit: usize,
    ) -> Result<Vec<CanonicalAddressRealmRecord>, StoreError>;
    async fn insert_canonical_pool(
        &self,
        pool: &CanonicalAddressPoolRecord,
    ) -> Result<(), StoreError>;
    async fn insert_subnet_bundle(
        &self,
        realm: &CanonicalAddressRealmRecord,
        pool: &CanonicalAddressPoolRecord,
        subnet: &SubnetRecord,
    ) -> Result<(), StoreError>;
    async fn list_canonical_pools(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalAddressPoolRecord>, StoreError>;
    async fn delete_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn update_canonical_pool(
        &self,
        project_id: &str,
        pool_id: &Uuid,
        expected_generation: u64,
        gateway: Option<std::net::Ipv4Addr>,
    ) -> Result<CanonicalAddressPoolRecord, StoreError>;
    async fn insert_canonical_endpoint(
        &self,
        endpoint: &CanonicalEndpointRecord,
    ) -> Result<(), StoreError>;
    async fn insert_canonical_endpoint_and_port(
        &self,
        endpoint: &CanonicalEndpointRecord,
        port: &PortRecord,
    ) -> Result<(), StoreError>;
    async fn list_canonical_endpoints(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalEndpointRecord>, StoreError>;
    async fn get_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<Option<CanonicalEndpointRecord>, StoreError>;
    async fn delete_canonical_endpoint(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn delete_canonical_endpoint_and_port(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn upsert_canonical_policy(
        &self,
        policy: &CanonicalNetworkPolicyRecord,
    ) -> Result<(), StoreError>;
    async fn list_canonical_policies(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<CanonicalNetworkPolicyRecord>, StoreError>;
    async fn delete_canonical_policy(
        &self,
        project_id: &str,
        policy_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn begin_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<CanonicalAddressRealmRecord, StoreError>;
    async fn finalize_canonical_realm_deletion(
        &self,
        project_id: &str,
        realm_id: &Uuid,
        expected_generation: u64,
    ) -> Result<(), StoreError>;
    async fn list_canonical_realm_bindings(
        &self,
        realm_id: &Uuid,
    ) -> Result<Vec<CanonicalRealmBindingRecord>, StoreError>;
    async fn delete_canonical_realm_binding(
        &self,
        binding: &CanonicalRealmBindingRecord,
        expected_realm_generation: u64,
    ) -> Result<(), StoreError>;
    async fn delete_canonical_realm(
        &self,
        project_id: &str,
        realm_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn delete_canonical_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<(), StoreError>;
    /// Atomically removes canonical network authority, tombstones its generic
    /// projection, and persists the secret-safe deletion audit.
    async fn delete_canonical_network_with_audit(
        &self,
        project_id: &str,
        network_id: &Uuid,
        operation_id: Uuid,
        audit: &AuditEventRecord,
    ) -> Result<(), StoreError>;
    async fn backfill_canonical_network_state(&self) -> Result<(), StoreError>;
    async fn allocate_network_address(
        &self,
        realm_id: &Uuid,
        project_id: &str,
        endpoint_id: &Uuid,
        operation_id: &str,
        prefix: &str,
    ) -> Result<NetworkAddressAllocationRecord, StoreError>;
    async fn release_network_address(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn insert_network_intent(&self, intent: &NetworkIntentRecord) -> Result<(), StoreError>;
    async fn list_network_intents(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkIntentRecord>, StoreError>;
    async fn get_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkIntentRecord>, StoreError>;
    async fn update_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        payload: &str,
        plan_fingerprint_sha256: Option<&str>,
        status: &str,
    ) -> Result<NetworkIntentRecord, StoreError>;
    async fn insert_network(&self, network: &NetworkRecord) -> Result<(), StoreError>;
    async fn list_networks(&self, project_id: &str) -> Result<Vec<NetworkRecord>, StoreError>;
    async fn get_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkRecord>, StoreError>;
    async fn delete_network(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;

    async fn insert_subnet(&self, subnet: &SubnetRecord) -> Result<(), StoreError>;
    async fn list_subnets(&self, project_id: &str) -> Result<Vec<SubnetRecord>, StoreError>;
    async fn list_subnets_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<SubnetRecord>, StoreError>;
    async fn get_subnet(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SubnetRecord>, StoreError>;
    async fn delete_subnet(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;
    async fn delete_subnet_bundle(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;
    async fn update_subnet(&self, subnet: &SubnetRecord) -> Result<(), StoreError>;
    async fn update_subnet_bundle(
        &self,
        subnet: &SubnetRecord,
        pool_id: &Uuid,
        expected_pool_generation: u64,
    ) -> Result<(), StoreError>;

    async fn insert_port(&self, port: &PortRecord) -> Result<(), StoreError>;
    async fn list_ports(&self, project_id: &str) -> Result<Vec<PortRecord>, StoreError>;
    async fn list_ports_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<PortRecord>, StoreError>;
    async fn get_port(&self, project_id: &str, id: &Uuid)
    -> Result<Option<PortRecord>, StoreError>;
    async fn get_port_by_id(&self, id: &Uuid) -> Result<Option<PortRecord>, StoreError>;
    async fn delete_port(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;
    async fn update_port_binding(
        &self,
        project_id: &str,
        id: &Uuid,
        binding_host: Option<&str>,
        binding_state: Option<&str>,
    ) -> Result<PortRecord, StoreError>;
    async fn update_port_name(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
    ) -> Result<PortRecord, StoreError>;
    async fn insert_security_group(&self, group: &SecurityGroupRecord) -> Result<(), StoreError>;
    async fn list_security_groups(
        &self,
        project_id: &str,
    ) -> Result<Vec<SecurityGroupRecord>, StoreError>;
    async fn get_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRecord>, StoreError>;
    async fn update_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
        description: &str,
    ) -> Result<SecurityGroupRecord, StoreError>;
    async fn delete_security_group(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError>;
    async fn insert_security_group_rule(
        &self,
        rule: &SecurityGroupRuleRecord,
    ) -> Result<(), StoreError>;
    async fn list_security_group_rules(
        &self,
        project_id: &str,
        group_id: &Uuid,
    ) -> Result<Vec<SecurityGroupRuleRecord>, StoreError>;
    async fn get_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRuleRecord>, StoreError>;
    async fn delete_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<(), StoreError>;
    async fn list_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: Option<&Uuid>,
    ) -> Result<Vec<SecurityGroupBindingRecord>, StoreError>;
    async fn replace_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
        group_ids: &[Uuid],
    ) -> Result<(), StoreError>;
}

/// Durable Placement-compatible provider inventory, allocation, and intent
/// records owned by the placement service: provider registration and state,
/// generation-guarded inventory refresh, allocation commit/release with
/// idempotent retries, allocation intents recorded before capacity is
/// committed, consumer reconciliation, and row-granular provider import.
///
/// This is a narrow port around the placement use cases, not a generic
/// persistence surface. Application code depends on this trait instead of on
/// the concrete `SqliteStore` adapter.
#[async_trait]
pub trait PlacementRepository: Send + Sync {
    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<PlacementProviderRecord>, StoreError>;
    async fn list_providers(&self) -> Result<Vec<PlacementProviderRecord>, StoreError>;
    /// Aggregate capacity in SQL. The limit bounds the number of resource
    /// classes returned and prevents provider/inventory fan-out in callers.
    async fn capacity_summary(
        &self,
        limit: usize,
    ) -> Result<(Vec<PlacementCapacityRecord>, u64, u64), StoreError>;
    async fn register_provider(
        &self,
        node_id: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError>;
    async fn sync_provider(
        &self,
        node_id: &str,
        state: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError>;
    async fn refresh_inventories(
        &self,
        provider_id: &str,
        expected_generation: u64,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError>;
    async fn set_provider_state(&self, provider_id: &str, state: &str) -> Result<(), StoreError>;
    async fn commit_allocation(
        &self,
        provider_id: &str,
        expected_generation: u64,
        allocation: &PlacementAllocationRecord,
    ) -> Result<PlacementAllocationRecord, StoreError>;
    async fn release_allocation(
        &self,
        provider_id: &str,
        allocation_id: &str,
    ) -> Result<(), StoreError>;
    async fn upsert_intent(
        &self,
        intent: &PlacementIntentRecord,
    ) -> Result<PlacementIntentRecord, StoreError>;
    async fn get_intent(
        &self,
        allocation_id: &str,
    ) -> Result<Option<PlacementIntentRecord>, StoreError>;
    async fn list_intents(&self) -> Result<Vec<PlacementIntentRecord>, StoreError>;
    async fn delete_intent(&self, allocation_id: &str) -> Result<(), StoreError>;
    async fn reconcile_consumers(
        &self,
        durable_consumer_ids: &[String],
    ) -> Result<PlacementReconcileRecord, StoreError>;
    async fn import_provider(&self, provider: &PlacementProviderRecord) -> Result<(), StoreError>;
}

/// The persistence surface of the compute application service.
///
/// Combines the reconciler's `DurableStore` semantics (resources, operations,
/// agent commands, artifact transfers, image overlays, provider references —
/// already consumed generically by `OperationJournal`) with the keypair,
/// volume-attachment, and recovery-list capabilities the compute service uses.
/// Application code depends on this port; the composition root chooses the
/// concrete adapter.
#[async_trait]
pub trait ComputeRepository:
    DurableStore + KeypairRepository + VolumeAttachmentRepository + QuotaRepository
{
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError>;
}
