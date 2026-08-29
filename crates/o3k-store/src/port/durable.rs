use async_trait::async_trait;
use sqlx::Row;
use uuid::Uuid;

use crate::domain::error::StoreError;
use crate::domain::records::{
    AgentCommandRecord, CanonicalOperationLifecycleUpdate, CanonicalOperationRecord,
    IdempotencyReservationRequest, ImageOverlayIdentity, ImageOverlayOwnershipRecord,
    ImageOverlayUpdate, ObservationUpdate, OperationRecord, ProviderReference, ResourceRecord,
};
use crate::domain::state::{
    AgentCommandState, CanonicalAcceptanceOutcome, IdempotencyReservation, OperationState,
};
use crate::{ArtifactTransferRecord, ArtifactTransferUpdate};

/// Generic durable parent/child relationship intent used by external service
/// composition. The record is intentionally service-neutral; service-owned
/// slot names are data, not schema.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceRelationshipRecord {
    pub parent_resource_id: Uuid,
    pub parent_resource_type: String,
    pub slot: String,
    pub expected_child_resource_type: String,
    pub child_resource_id: Option<Uuid>,
    pub ownership: String,
    pub parent_operation_id: Uuid,
    pub child_operation_id: Option<Uuid>,
    pub owner_scope: String,
    pub state: String,
    pub fingerprint: String,
}

/// Generic durable parent/child relationship port.
///
/// This deliberately contains no service-specific vocabulary.  External
/// controllers use it through the composition boundary so recovery does not
/// depend on controller-local memory.
#[async_trait]
pub trait RelationshipRepository: Send + Sync {
    async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError>;
    async fn get_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError>;
    async fn list_relationships(
        &self,
        parent_resource_id: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError>;
    async fn bind_relationship(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        child_resource_id: Uuid,
        child_operation_id: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError>;
    async fn set_relationship_state(
        &self,
        parent_resource_id: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError>;
}

pub(crate) fn relationship_from_row(
    row: &sqlx::sqlite::SqliteRow,
) -> Result<ResourceRelationshipRecord, StoreError> {
    let parse = |value: String| Uuid::parse_str(&value).map_err(StoreError::InvalidUuid);
    Ok(ResourceRelationshipRecord {
        parent_resource_id: parse(
            row.try_get("parent_resource_id")
                .map_err(StoreError::Database)?,
        )?,
        parent_resource_type: row
            .try_get("parent_resource_type")
            .map_err(StoreError::Database)?,
        slot: row.try_get("slot").map_err(StoreError::Database)?,
        expected_child_resource_type: row
            .try_get("expected_child_resource_type")
            .map_err(StoreError::Database)?,
        child_resource_id: row
            .try_get::<Option<String>, _>("child_resource_id")
            .map_err(StoreError::Database)?
            .map(parse)
            .transpose()?,
        ownership: row.try_get("ownership").map_err(StoreError::Database)?,
        parent_operation_id: parse(
            row.try_get("parent_operation_id")
                .map_err(StoreError::Database)?,
        )?,
        child_operation_id: row
            .try_get::<Option<String>, _>("child_operation_id")
            .map_err(StoreError::Database)?
            .map(parse)
            .transpose()?,
        owner_scope: row.try_get("owner_scope").map_err(StoreError::Database)?,
        state: row.try_get("state").map_err(StoreError::Database)?,
        fingerprint: row.try_get("fingerprint").map_err(StoreError::Database)?,
    })
}

pub(crate) const RELATIONSHIP_RESERVED: &str = "reserved";
pub(crate) const RELATIONSHIP_BOUND: &str = "bound";
pub(crate) const RELATIONSHIP_DELETING: &str = "deleting";
pub(crate) const RELATIONSHIP_DELETED: &str = "deleted";
pub(crate) const RELATIONSHIP_UNKNOWN: &str = "unknown";

#[async_trait]
pub trait DurableStore: Send + Sync {
    async fn insert_resource(&self, resource: &ResourceRecord) -> Result<(), StoreError>;
    async fn get_resource(&self, id: Uuid) -> Result<ResourceRecord, StoreError>;
    async fn list_resources(
        &self,
        project_id: &str,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError>;
    async fn update_resource(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError>;
    async fn update_resource_from_observation(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError>;
    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError>;
    async fn reserve_idempotent_operation(
        &self,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError>;
    /// Atomically creates an operation and its idempotency reservation.
    /// Existing reservations are only replayable when their operation still
    /// exists; a dangling reservation is treated as corruption.
    async fn create_or_replay_idempotent_operation(
        &self,
        operation: &OperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError>;
    /// Atomically creates the complete public canonical operation triplet:
    /// durable operation, canonical metadata, and idempotency reservation.
    async fn create_or_replay_canonical_idempotent_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError>;
    /// Atomically records/replays canonical operation metadata for a
    /// canonical resource whose authoritative row is owned by a
    /// service-specific table rather than the generic `resources` index.
    /// This keeps the shared Operation/idempotency contract while avoiding a
    /// second desired-state authority for those resources.
    async fn create_or_replay_canonical_scoped_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError>;
    async fn create_or_replay_canonical_resource_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<CanonicalAcceptanceOutcome, StoreError>;
    async fn create_or_replay_canonical_lifecycle_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<CanonicalAcceptanceOutcome, StoreError>;
    async fn get_operation(&self, id: Uuid) -> Result<OperationRecord, StoreError>;
    async fn get_canonical_operation(
        &self,
        id: Uuid,
    ) -> Result<CanonicalOperationRecord, StoreError>;
    async fn update_canonical_operation_lifecycle(
        &self,
        id: Uuid,
        update: &CanonicalOperationLifecycleUpdate,
    ) -> Result<CanonicalOperationRecord, StoreError>;
    async fn update_operation(
        &self,
        id: Uuid,
        state: OperationState,
        provider_operation_id: Option<&str>,
        error_category: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationRecord, StoreError>;
    /// Lists lifecycle-kind operations (`kind LIKE 'lifecycle:%'`) that have
    /// not reached a terminal state (`succeeded`/`failed`, the reconciler's
    /// terminal predicate). The periodic lifecycle-convergence sweep drives
    /// exactly these rows: an unknown delete/action outcome can leave a
    /// lifecycle operation non-terminal with no event-stream path ever
    /// advancing it again (issue #88 B1).
    async fn list_non_terminal_lifecycle_operations(
        &self,
    ) -> Result<Vec<OperationRecord>, StoreError>;
    async fn attach_provider_reference(
        &self,
        reference: &ProviderReference,
    ) -> Result<(), StoreError>;
    async fn get_provider_reference(
        &self,
        resource_id: Uuid,
        provider_name: &str,
    ) -> Result<ProviderReference, StoreError>;
    async fn insert_agent_command(
        &self,
        command: &AgentCommandRecord,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command(&self, command_id: &str) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn get_agent_command_by_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn update_agent_command(
        &self,
        command_id: &str,
        state: AgentCommandState,
        accepted_sequence: u64,
        last_sequence: u64,
        provider_operation_id: Option<&str>,
        provider_resource_id: Option<&str>,
    ) -> Result<AgentCommandRecord, StoreError>;
    async fn list_recoverable_agent_commands(&self) -> Result<Vec<AgentCommandRecord>, StoreError>;
    async fn insert_artifact_transfer(
        &self,
        transfer: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn get_artifact_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn rebind_artifact_transfer_epoch(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        new_agent_epoch: &str,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError>;
    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError>;
    /// Marks every artifact transfer whose owning operation has already
    /// reached a terminal state (`succeeded`/`failed`, the reconciler's
    /// terminal predicate) as `expired`, and returns the number of rows
    /// expired. An operation can terminalize while its `offered`/`receiving`
    /// handshake rows are still non-terminal, and no per-operation path ever
    /// advances those rows again (issue #88). Idempotent and cheap: repeated
    /// runs expire nothing, and `committed`/`rejected`/`expired` rows are
    /// never touched.
    async fn expire_transfers_of_terminal_operations(&self) -> Result<u64, StoreError>;
    async fn insert_image_overlay(
        &self,
        overlay: &ImageOverlayOwnershipRecord,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn get_image_overlay(
        &self,
        overlay_id: &str,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn update_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
        update: ImageOverlayUpdate,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn list_image_overlays(
        &self,
        resource_id: Uuid,
    ) -> Result<Vec<ImageOverlayOwnershipRecord>, StoreError>;
    async fn count_image_overlay_references(
        &self,
        base_sha256: &str,
        base_format: &str,
    ) -> Result<u64, StoreError>;
    async fn delete_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError>;
    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError>;
    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<(), StoreError>;
    /// Revives a resource row left in a terminal `DELETED` observed state by a
    /// completed lifecycle into a fresh create intent, recording the fresh
    /// lifecycle operation in the same transaction. This is the recreate path
    /// for a create whose deterministic identity collides with a COMPLETED
    /// prior lifecycle (the one-line TestLab recreate contract): the row
    /// update and the operation insert persist atomically, so a crash can
    /// never strand a pending operation without its resource intent or vice
    /// versa, and the placement allocation referenced by the revived intent
    /// must still exist (the ASR-018 ordering invariant, identical to
    /// `insert_resource_and_operation`). The generation fence rejects a
    /// concurrent writer that already advanced the row.
    #[allow(clippy::too_many_arguments)]
    async fn revive_resource_and_operation(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
        operation: &OperationRecord,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError>;
    async fn readiness_check(&self) -> Result<(), StoreError>;
}
