//! Unified O3K store abstraction supporting both SQLite and PostgreSQL backends.

use async_trait::async_trait;
use std::path::Path;
use uuid::Uuid;

use o3k_kernel::{
    LimitKey, LimitValue, OwnershipScope, Reservation, ReservationId, ResourceAmount, Usage,
};

use crate::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferUpdate,
    CanonicalOperationRecord, ComputeRepository, ControllerEpoch, ControllerId, ControllerSession,
    CoordinationRepository, DatabaseHealth, DurableStore, FencingToken, IdempotencyReservation,
    IdempotencyReservationRequest, IdentityRepository, ImageMetadataRecord, ImageOverlayIdentity,
    ImageOverlayOwnershipRecord, ImageOverlayUpdate, ImageRepository, KeypairRecord,
    KeypairRepository, KeystoneDomainRecord, KeystoneEndpointRecord, KeystoneProjectRecord,
    KeystoneRegionRecord, KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord,
    KeystoneUserRecord, LeaseAcquireOutcome, NetworkAddressAllocationRecord, NetworkIntentRecord,
    NetworkRecord, NetworkRepository, ObservationUpdate, OperationRecord, OperationState,
    PlacementAllocationRecord, PlacementIntentRecord, PlacementInventoryRecord,
    PlacementProviderRecord, PlacementReconcileRecord, PlacementRepository, PortRecord,
    PostgresStore, ProviderReference, ResourceRecord, SecurityGroupBindingRecord,
    SecurityGroupRecord, SecurityGroupRuleRecord, SnapshotRecord, SqliteStore,
    StorageBackendRecord, StorageRepository, StoreError, SubnetRecord, VolumeAttachmentRecord,
    VolumeAttachmentRecordV1, VolumeAttachmentRepository, VolumeRecord, WorkLease,
    quota::QuotaRepository,
};

#[derive(Clone, Debug)]
pub enum O3kStore {
    Sqlite(SqliteStore),
    Postgres(PostgresStore),
}

impl O3kStore {
    pub async fn connect_sqlite_file(path: &Path) -> Result<Self, StoreError> {
        let store = SqliteStore::connect_file(path).await?;
        Ok(Self::Sqlite(store))
    }

    pub async fn connect_sqlite_memory() -> Result<Self, StoreError> {
        let store = SqliteStore::connect("sqlite::memory:").await?;
        Ok(Self::Sqlite(store))
    }

    pub async fn connect_postgres(url: &str) -> Result<Self, StoreError> {
        let store = PostgresStore::connect(url).await?;
        Ok(Self::Postgres(store))
    }

    pub async fn database_health(&self) -> Result<DatabaseHealth, StoreError> {
        match self {
            Self::Sqlite(s) => s.database_health().await,
            Self::Postgres(s) => s.database_health().await,
        }
    }

    pub async fn readiness_check(&self) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.readiness_check().await,
            Self::Postgres(s) => s.readiness_check().await,
        }
    }
}

#[async_trait]
impl StorageRepository for O3kStore {
    async fn insert_storage_backend(
        &self,
        record: &StorageBackendRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_storage_backend(record).await,
            Self::Postgres(store) => store.insert_storage_backend(record).await,
        }
    }

    async fn get_storage_backend(
        &self,
        id: &str,
    ) -> Result<Option<StorageBackendRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_storage_backend(id).await,
            Self::Postgres(store) => store.get_storage_backend(id).await,
        }
    }

    async fn list_storage_backends(&self) -> Result<Vec<StorageBackendRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_storage_backends().await,
            Self::Postgres(store) => store.list_storage_backends().await,
        }
    }

    async fn insert_volume(&self, record: &VolumeRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_volume(record).await,
            Self::Postgres(store) => store.insert_volume(record).await,
        }
    }

    async fn get_volume(&self, id: Uuid) -> Result<Option<VolumeRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_volume(id).await,
            Self::Postgres(store) => store.get_volume(id).await,
        }
    }

    async fn list_volumes(&self, project_id: &str) -> Result<Vec<VolumeRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_volumes(project_id).await,
            Self::Postgres(store) => store.list_volumes(project_id).await,
        }
    }

    async fn update_volume(
        &self,
        expected_generation: u64,
        record: &VolumeRecord,
    ) -> Result<VolumeRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.update_volume(expected_generation, record).await,
            Self::Postgres(store) => store.update_volume(expected_generation, record).await,
        }
    }

    async fn delete_volume(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_volume(project_id, id).await,
            Self::Postgres(store) => store.delete_volume(project_id, id).await,
        }
    }

    async fn insert_volume_attachment_v1(
        &self,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_volume_attachment_v1(record).await,
            Self::Postgres(store) => store.insert_volume_attachment_v1(record).await,
        }
    }

    async fn get_volume_attachment_v1(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecordV1>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_volume_attachment_v1(id).await,
            Self::Postgres(store) => store.get_volume_attachment_v1(id).await,
        }
    }

    async fn list_volume_attachments_v1(
        &self,
        project_id: &str,
    ) -> Result<Vec<VolumeAttachmentRecordV1>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_volume_attachments_v1(project_id).await,
            Self::Postgres(store) => store.list_volume_attachments_v1(project_id).await,
        }
    }

    async fn update_volume_attachment_v1(
        &self,
        expected_generation: u64,
        record: &VolumeAttachmentRecordV1,
    ) -> Result<VolumeAttachmentRecordV1, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .update_volume_attachment_v1(expected_generation, record)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .update_volume_attachment_v1(expected_generation, record)
                    .await
            }
        }
    }

    async fn delete_volume_attachment_v1(
        &self,
        project_id: &str,
        id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_volume_attachment_v1(project_id, id).await,
            Self::Postgres(store) => store.delete_volume_attachment_v1(project_id, id).await,
        }
    }

    async fn insert_snapshot(&self, record: &SnapshotRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.insert_snapshot(record).await,
            Self::Postgres(store) => store.insert_snapshot(record).await,
        }
    }

    async fn get_snapshot(&self, id: Uuid) -> Result<Option<SnapshotRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_snapshot(id).await,
            Self::Postgres(store) => store.get_snapshot(id).await,
        }
    }

    async fn list_snapshots(&self, project_id: &str) -> Result<Vec<SnapshotRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_snapshots(project_id).await,
            Self::Postgres(store) => store.list_snapshots(project_id).await,
        }
    }

    async fn update_snapshot(
        &self,
        expected_generation: u64,
        record: &SnapshotRecord,
    ) -> Result<SnapshotRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.update_snapshot(expected_generation, record).await,
            Self::Postgres(store) => store.update_snapshot(expected_generation, record).await,
        }
    }

    async fn delete_snapshot(&self, project_id: &str, id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(store) => store.delete_snapshot(project_id, id).await,
            Self::Postgres(store) => store.delete_snapshot(project_id, id).await,
        }
    }
}

#[async_trait]
impl DurableStore for O3kStore {
    async fn insert_resource(&self, resource: &ResourceRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_resource(resource).await,
            Self::Postgres(s) => s.insert_resource(resource).await,
        }
    }

    async fn get_resource(&self, id: Uuid) -> Result<ResourceRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_resource(id).await,
            Self::Postgres(s) => s.get_resource(id).await,
        }
    }

    async fn list_resources(
        &self,
        project_id: &str,
        kind: &str,
    ) -> Result<Vec<ResourceRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_resources(project_id, kind).await,
            Self::Postgres(s) => s.list_resources(project_id, kind).await,
        }
    }

    async fn update_resource(
        &self,
        id: Uuid,
        expected_generation: i64,
        desired_state: &str,
        observed_state: &str,
        observed_generation: i64,
        provider_id: Option<&str>,
    ) -> Result<ResourceRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_resource(
                    id,
                    expected_generation,
                    desired_state,
                    observed_state,
                    observed_generation,
                    provider_id,
                )
                .await
            }
            Self::Postgres(s) => {
                s.update_resource(
                    id,
                    expected_generation,
                    desired_state,
                    observed_state,
                    observed_generation,
                    provider_id,
                )
                .await
            }
        }
    }

    async fn update_resource_from_observation(
        &self,
        id: Uuid,
        update: &ObservationUpdate<'_>,
    ) -> Result<ResourceRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.update_resource_from_observation(id, update).await,
            Self::Postgres(s) => s.update_resource_from_observation(id, update).await,
        }
    }

    async fn insert_operation(&self, operation: &OperationRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_operation(operation).await,
            Self::Postgres(s) => s.insert_operation(operation).await,
        }
    }

    async fn reserve_idempotent_operation(
        &self,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        match self {
            Self::Sqlite(s) => s.reserve_idempotent_operation(request).await,
            Self::Postgres(s) => s.reserve_idempotent_operation(request).await,
        }
    }

    async fn create_or_replay_idempotent_operation(
        &self,
        operation: &OperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.create_or_replay_idempotent_operation(operation, request)
                    .await
            }
            Self::Postgres(s) => {
                s.create_or_replay_idempotent_operation(operation, request)
                    .await
            }
        }
    }

    async fn create_or_replay_canonical_idempotent_operation(
        &self,
        operation: &OperationRecord,
        canonical: &CanonicalOperationRecord,
        request: &IdempotencyReservationRequest,
    ) -> Result<IdempotencyReservation, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .create_or_replay_canonical_idempotent_operation(operation, canonical, request)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .create_or_replay_canonical_idempotent_operation(operation, canonical, request)
                    .await
            }
        }
    }

    async fn get_operation(&self, id: Uuid) -> Result<OperationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_operation(id).await,
            Self::Postgres(s) => s.get_operation(id).await,
        }
    }

    async fn get_canonical_operation(
        &self,
        id: Uuid,
    ) -> Result<CanonicalOperationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_canonical_operation(id).await,
            Self::Postgres(s) => s.get_canonical_operation(id).await,
        }
    }

    async fn update_operation(
        &self,
        id: Uuid,
        state: OperationState,
        provider_operation_id: Option<&str>,
        error_category: Option<&str>,
        error_message: Option<&str>,
    ) -> Result<OperationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_operation(
                    id,
                    state,
                    provider_operation_id,
                    error_category,
                    error_message,
                )
                .await
            }
            Self::Postgres(s) => {
                s.update_operation(
                    id,
                    state,
                    provider_operation_id,
                    error_category,
                    error_message,
                )
                .await
            }
        }
    }

    async fn list_non_terminal_lifecycle_operations(
        &self,
    ) -> Result<Vec<OperationRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_non_terminal_lifecycle_operations().await,
            Self::Postgres(s) => s.list_non_terminal_lifecycle_operations().await,
        }
    }

    async fn attach_provider_reference(
        &self,
        reference: &ProviderReference,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.attach_provider_reference(reference).await,
            Self::Postgres(s) => s.attach_provider_reference(reference).await,
        }
    }

    async fn get_provider_reference(
        &self,
        resource_id: Uuid,
        provider_name: &str,
    ) -> Result<ProviderReference, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_provider_reference(resource_id, provider_name).await,
            Self::Postgres(s) => s.get_provider_reference(resource_id, provider_name).await,
        }
    }

    async fn insert_agent_command(
        &self,
        command: &AgentCommandRecord,
    ) -> Result<AgentCommandRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_agent_command(command).await,
            Self::Postgres(s) => s.insert_agent_command(command).await,
        }
    }

    async fn get_agent_command(&self, command_id: &str) -> Result<AgentCommandRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_agent_command(command_id).await,
            Self::Postgres(s) => s.get_agent_command(command_id).await,
        }
    }

    async fn get_agent_command_by_idempotency_key(
        &self,
        idempotency_key: &str,
    ) -> Result<AgentCommandRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_agent_command_by_idempotency_key(idempotency_key)
                    .await
            }
            Self::Postgres(s) => {
                s.get_agent_command_by_idempotency_key(idempotency_key)
                    .await
            }
        }
    }

    async fn get_agent_command_by_operation(
        &self,
        operation_id: Uuid,
    ) -> Result<AgentCommandRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_agent_command_by_operation(operation_id).await,
            Self::Postgres(s) => s.get_agent_command_by_operation(operation_id).await,
        }
    }

    async fn update_agent_command(
        &self,
        command_id: &str,
        state: AgentCommandState,
        accepted_sequence: u64,
        last_sequence: u64,
        provider_operation_id: Option<&str>,
        provider_resource_id: Option<&str>,
    ) -> Result<AgentCommandRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_agent_command(
                    command_id,
                    state,
                    accepted_sequence,
                    last_sequence,
                    provider_operation_id,
                    provider_resource_id,
                )
                .await
            }
            Self::Postgres(s) => {
                s.update_agent_command(
                    command_id,
                    state,
                    accepted_sequence,
                    last_sequence,
                    provider_operation_id,
                    provider_resource_id,
                )
                .await
            }
        }
    }

    async fn list_recoverable_agent_commands(&self) -> Result<Vec<AgentCommandRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_recoverable_agent_commands().await,
            Self::Postgres(s) => s.list_recoverable_agent_commands().await,
        }
    }

    async fn insert_artifact_transfer(
        &self,
        transfer: &ArtifactTransferRecord,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_artifact_transfer(transfer).await,
            Self::Postgres(s) => s.insert_artifact_transfer(transfer).await,
        }
    }

    async fn get_artifact_transfer(
        &self,
        transfer_id: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_artifact_transfer(transfer_id).await,
            Self::Postgres(s) => s.get_artifact_transfer(transfer_id).await,
        }
    }

    async fn rebind_artifact_transfer_epoch(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        new_agent_epoch: &str,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.rebind_artifact_transfer_epoch(transfer_id, expected_agent_epoch, new_agent_epoch)
                    .await
            }
            Self::Postgres(s) => {
                s.rebind_artifact_transfer_epoch(transfer_id, expected_agent_epoch, new_agent_epoch)
                    .await
            }
        }
    }

    async fn update_artifact_transfer(
        &self,
        transfer_id: &str,
        expected_agent_epoch: &str,
        update: ArtifactTransferUpdate,
    ) -> Result<ArtifactTransferRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_artifact_transfer(transfer_id, expected_agent_epoch, update)
                    .await
            }
            Self::Postgres(s) => {
                s.update_artifact_transfer(transfer_id, expected_agent_epoch, update)
                    .await
            }
        }
    }

    async fn list_recoverable_artifact_transfers(
        &self,
    ) -> Result<Vec<ArtifactTransferRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_recoverable_artifact_transfers().await,
            Self::Postgres(s) => s.list_recoverable_artifact_transfers().await,
        }
    }

    async fn expire_transfers_of_terminal_operations(&self) -> Result<u64, StoreError> {
        match self {
            Self::Sqlite(s) => s.expire_transfers_of_terminal_operations().await,
            Self::Postgres(s) => s.expire_transfers_of_terminal_operations().await,
        }
    }

    async fn insert_image_overlay(
        &self,
        overlay: &ImageOverlayOwnershipRecord,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_image_overlay(overlay).await,
            Self::Postgres(s) => s.insert_image_overlay(overlay).await,
        }
    }

    async fn get_image_overlay(
        &self,
        overlay_id: &str,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_image_overlay(overlay_id).await,
            Self::Postgres(s) => s.get_image_overlay(overlay_id).await,
        }
    }

    async fn update_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
        update: ImageOverlayUpdate,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_image_overlay(overlay_id, expected_identity, update)
                    .await
            }
            Self::Postgres(s) => {
                s.update_image_overlay(overlay_id, expected_identity, update)
                    .await
            }
        }
    }

    async fn list_image_overlays(
        &self,
        resource_id: Uuid,
    ) -> Result<Vec<ImageOverlayOwnershipRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_image_overlays(resource_id).await,
            Self::Postgres(s) => s.list_image_overlays(resource_id).await,
        }
    }

    async fn count_image_overlay_references(
        &self,
        base_sha256: &str,
        base_format: &str,
    ) -> Result<u64, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.count_image_overlay_references(base_sha256, base_format)
                    .await
            }
            Self::Postgres(s) => {
                s.count_image_overlay_references(base_sha256, base_format)
                    .await
            }
        }
    }

    async fn delete_image_overlay(
        &self,
        overlay_id: &str,
        expected_identity: &ImageOverlayIdentity,
    ) -> Result<ImageOverlayOwnershipRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_image_overlay(overlay_id, expected_identity).await,
            Self::Postgres(s) => s.delete_image_overlay(overlay_id, expected_identity).await,
        }
    }

    async fn increment_operation_retry(&self, operation_id: Uuid) -> Result<u8, StoreError> {
        match self {
            Self::Sqlite(s) => s.increment_operation_retry(operation_id).await,
            Self::Postgres(s) => s.increment_operation_retry(operation_id).await,
        }
    }

    async fn insert_resource_and_operation(
        &self,
        resource: &ResourceRecord,
        operation: &OperationRecord,
        expected_placement_allocation_id: Option<&str>,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.insert_resource_and_operation(
                    resource,
                    operation,
                    expected_placement_allocation_id,
                )
                .await
            }
            Self::Postgres(s) => {
                s.insert_resource_and_operation(
                    resource,
                    operation,
                    expected_placement_allocation_id,
                )
                .await
            }
        }
    }

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
    ) -> Result<ResourceRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.revive_resource_and_operation(
                    id,
                    expected_generation,
                    desired_state,
                    observed_state,
                    observed_generation,
                    provider_id,
                    operation,
                    expected_placement_allocation_id,
                )
                .await
            }
            Self::Postgres(s) => {
                s.revive_resource_and_operation(
                    id,
                    expected_generation,
                    desired_state,
                    observed_state,
                    observed_generation,
                    provider_id,
                    operation,
                    expected_placement_allocation_id,
                )
                .await
            }
        }
    }

    async fn readiness_check(&self) -> Result<(), StoreError> {
        self.readiness_check().await
    }
}

#[async_trait]
impl IdentityRepository for O3kStore {
    async fn insert_keystone_domain(
        &self,
        domain: &KeystoneDomainRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_domain(domain).await,
            Self::Postgres(s) => s.insert_keystone_domain(domain).await,
        }
    }

    async fn list_keystone_domains(&self) -> Result<Vec<KeystoneDomainRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_domains().await,
            Self::Postgres(s) => s.list_keystone_domains().await,
        }
    }

    async fn insert_keystone_project(
        &self,
        project: &KeystoneProjectRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_project(project).await,
            Self::Postgres(s) => s.insert_keystone_project(project).await,
        }
    }

    async fn list_keystone_projects(&self) -> Result<Vec<KeystoneProjectRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_projects().await,
            Self::Postgres(s) => s.list_keystone_projects().await,
        }
    }

    async fn insert_keystone_user(&self, user: &KeystoneUserRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_user(user).await,
            Self::Postgres(s) => s.insert_keystone_user(user).await,
        }
    }

    async fn list_keystone_users(&self) -> Result<Vec<KeystoneUserRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_users().await,
            Self::Postgres(s) => s.list_keystone_users().await,
        }
    }

    async fn insert_keystone_role(&self, role: &KeystoneRoleRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_role(role).await,
            Self::Postgres(s) => s.insert_keystone_role(role).await,
        }
    }

    async fn list_keystone_roles(&self) -> Result<Vec<KeystoneRoleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_roles().await,
            Self::Postgres(s) => s.list_keystone_roles().await,
        }
    }

    async fn insert_keystone_role_assignment(
        &self,
        assignment: &KeystoneRoleAssignmentRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_role_assignment(assignment).await,
            Self::Postgres(s) => s.insert_keystone_role_assignment(assignment).await,
        }
    }

    async fn list_keystone_role_assignments(
        &self,
    ) -> Result<Vec<KeystoneRoleAssignmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_role_assignments().await,
            Self::Postgres(s) => s.list_keystone_role_assignments().await,
        }
    }

    async fn insert_keystone_service(
        &self,
        service: &KeystoneServiceRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_service(service).await,
            Self::Postgres(s) => s.insert_keystone_service(service).await,
        }
    }

    async fn list_keystone_services(&self) -> Result<Vec<KeystoneServiceRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_services().await,
            Self::Postgres(s) => s.list_keystone_services().await,
        }
    }

    async fn insert_keystone_endpoint(
        &self,
        endpoint: &KeystoneEndpointRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_endpoint(endpoint).await,
            Self::Postgres(s) => s.insert_keystone_endpoint(endpoint).await,
        }
    }

    async fn list_keystone_endpoints(&self) -> Result<Vec<KeystoneEndpointRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_endpoints().await,
            Self::Postgres(s) => s.list_keystone_endpoints().await,
        }
    }

    async fn insert_keystone_region(
        &self,
        region: &KeystoneRegionRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keystone_region(region).await,
            Self::Postgres(s) => s.insert_keystone_region(region).await,
        }
    }

    async fn list_keystone_regions(&self) -> Result<Vec<KeystoneRegionRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keystone_regions().await,
            Self::Postgres(s) => s.list_keystone_regions().await,
        }
    }
}

#[async_trait]
impl KeypairRepository for O3kStore {
    async fn insert_keypair(&self, keypair: &KeypairRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_keypair(keypair).await,
            Self::Postgres(s) => s.insert_keypair(keypair).await,
        }
    }

    async fn list_keypairs(
        &self,
        user_id: &str,
        project_id: &str,
    ) -> Result<Vec<KeypairRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_keypairs(user_id, project_id).await,
            Self::Postgres(s) => s.list_keypairs(user_id, project_id).await,
        }
    }

    async fn get_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<KeypairRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_keypair(user_id, project_id, name).await,
            Self::Postgres(s) => s.get_keypair(user_id, project_id, name).await,
        }
    }

    async fn delete_keypair(
        &self,
        user_id: &str,
        project_id: &str,
        name: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_keypair(user_id, project_id, name).await,
            Self::Postgres(s) => s.delete_keypair(user_id, project_id, name).await,
        }
    }

    async fn attach_server_keypair(
        &self,
        server_id: Uuid,
        keypair_id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.attach_server_keypair(server_id, keypair_id).await,
            Self::Postgres(s) => s.attach_server_keypair(server_id, keypair_id).await,
        }
    }

    async fn detach_server_keypair(&self, server_id: Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.detach_server_keypair(server_id).await,
            Self::Postgres(s) => s.detach_server_keypair(server_id).await,
        }
    }

    async fn get_server_keypair_name(&self, server_id: Uuid) -> Result<Option<String>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_server_keypair_name(server_id).await,
            Self::Postgres(s) => s.get_server_keypair_name(server_id).await,
        }
    }
}

#[async_trait]
impl VolumeAttachmentRepository for O3kStore {
    async fn insert_volume_attachment(
        &self,
        record: &VolumeAttachmentRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_volume_attachment(record).await,
            Self::Postgres(s) => s.insert_volume_attachment(record).await,
        }
    }

    async fn update_volume_attachment_phase(
        &self,
        id: Uuid,
        status: &str,
        error: Option<&str>,
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.update_volume_attachment_phase(id, status, error).await,
            Self::Postgres(s) => s.update_volume_attachment_phase(id, status, error).await,
        }
    }

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
    ) -> Result<VolumeAttachmentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_volume_attachment_outcome(
                    id,
                    status,
                    cinder_attachment_id,
                    connector_host,
                    connector_ip,
                    connector_initiator,
                    driver_volume_type,
                    target_iqn,
                    target_portal,
                    target_lun,
                    connection_info_digest,
                    device,
                )
                .await
            }
            Self::Postgres(s) => {
                s.update_volume_attachment_outcome(
                    id,
                    status,
                    cinder_attachment_id,
                    connector_host,
                    connector_ip,
                    connector_initiator,
                    driver_volume_type,
                    target_iqn,
                    target_portal,
                    target_lun,
                    connection_info_digest,
                    device,
                )
                .await
            }
        }
    }

    async fn get_volume_attachment_by_id(
        &self,
        id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment_by_id(id).await,
            Self::Postgres(s) => s.get_volume_attachment_by_id(id).await,
        }
    }

    async fn get_volume_attachment_by_volume(
        &self,
        volume_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment_by_volume(volume_id).await,
            Self::Postgres(s) => s.get_volume_attachment_by_volume(volume_id).await,
        }
    }

    async fn get_volume_attachment_by_volume_for_server(
        &self,
        volume_id: Uuid,
        server_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_volume_attachment_by_volume_for_server(volume_id, server_id)
                    .await
            }
            Self::Postgres(s) => {
                s.get_volume_attachment_by_volume_for_server(volume_id, server_id)
                    .await
            }
        }
    }

    async fn get_volume_attachment_by_idempotency(
        &self,
        idempotency_key: &str,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.get_volume_attachment_by_idempotency(idempotency_key)
                    .await
            }
            Self::Postgres(s) => {
                s.get_volume_attachment_by_idempotency(idempotency_key)
                    .await
            }
        }
    }

    async fn list_volume_attachments_by_status(
        &self,
        terminal: &[&str],
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_volume_attachments_by_status(terminal).await,
            Self::Postgres(s) => s.list_volume_attachments_by_status(terminal).await,
        }
    }

    async fn list_volume_attachments(
        &self,
        server_id: Uuid,
    ) -> Result<Vec<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_volume_attachments(server_id).await,
            Self::Postgres(s) => s.list_volume_attachments(server_id).await,
        }
    }

    async fn get_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<Option<VolumeAttachmentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_volume_attachment(server_id, attachment_id).await,
            Self::Postgres(s) => s.get_volume_attachment(server_id, attachment_id).await,
        }
    }

    async fn delete_volume_attachment(
        &self,
        server_id: Uuid,
        attachment_id: Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_volume_attachment(server_id, attachment_id).await,
            Self::Postgres(s) => s.delete_volume_attachment(server_id, attachment_id).await,
        }
    }
}

#[async_trait]
impl ImageRepository for O3kStore {
    async fn insert_image(&self, image: &ImageMetadataRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_image(image).await,
            Self::Postgres(s) => s.insert_image(image).await,
        }
    }

    async fn list_images(&self, project_id: &str) -> Result<Vec<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_images(project_id).await,
            Self::Postgres(s) => s.list_images(project_id).await,
        }
    }

    async fn get_image(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<ImageMetadataRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_image(project_id, id).await,
            Self::Postgres(s) => s.get_image(project_id, id).await,
        }
    }

    async fn activate_image(
        &self,
        project_id: &str,
        id: &Uuid,
        size: u64,
        checksum: &str,
    ) -> Result<ImageMetadataRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.activate_image(project_id, id, size, checksum).await,
            Self::Postgres(s) => s.activate_image(project_id, id, size, checksum).await,
        }
    }

    async fn delete_image(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_image(project_id, id).await,
            Self::Postgres(s) => s.delete_image(project_id, id).await,
        }
    }
}

#[async_trait]
impl NetworkRepository for O3kStore {
    async fn allocate_network_address(
        &self,
        realm_id: &Uuid,
        project_id: &str,
        endpoint_id: &Uuid,
        operation_id: &str,
        prefix: &str,
    ) -> Result<NetworkAddressAllocationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.allocate_network_address(realm_id, project_id, endpoint_id, operation_id, prefix)
                    .await
            }
            Self::Postgres(s) => {
                s.allocate_network_address(realm_id, project_id, endpoint_id, operation_id, prefix)
                    .await
            }
        }
    }

    async fn release_network_address(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_network_address(project_id, endpoint_id).await,
            Self::Postgres(s) => s.release_network_address(project_id, endpoint_id).await,
        }
    }

    async fn insert_network_intent(&self, intent: &NetworkIntentRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_network_intent(intent).await,
            Self::Postgres(s) => s.insert_network_intent(intent).await,
        }
    }

    async fn list_network_intents(
        &self,
        project_id: &str,
    ) -> Result<Vec<NetworkIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_network_intents(project_id).await,
            Self::Postgres(s) => s.list_network_intents(project_id).await,
        }
    }

    async fn get_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_network_intent(project_id, id).await,
            Self::Postgres(s) => s.get_network_intent(project_id, id).await,
        }
    }

    async fn update_network_intent(
        &self,
        project_id: &str,
        id: &Uuid,
        expected_generation: u64,
        payload: &str,
        plan_fingerprint_sha256: Option<&str>,
        status: &str,
    ) -> Result<NetworkIntentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_network_intent(
                    project_id,
                    id,
                    expected_generation,
                    payload,
                    plan_fingerprint_sha256,
                    status,
                )
                .await
            }
            Self::Postgres(s) => {
                s.update_network_intent(
                    project_id,
                    id,
                    expected_generation,
                    payload,
                    plan_fingerprint_sha256,
                    status,
                )
                .await
            }
        }
    }

    async fn insert_network(&self, network: &NetworkRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_network(network).await,
            Self::Postgres(s) => s.insert_network(network).await,
        }
    }

    async fn list_networks(&self, project_id: &str) -> Result<Vec<NetworkRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_networks(project_id).await,
            Self::Postgres(s) => s.list_networks(project_id).await,
        }
    }

    async fn get_network(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<NetworkRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_network(project_id, id).await,
            Self::Postgres(s) => s.get_network(project_id, id).await,
        }
    }

    async fn delete_network(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_network(project_id, id).await,
            Self::Postgres(s) => s.delete_network(project_id, id).await,
        }
    }

    async fn insert_subnet(&self, subnet: &SubnetRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_subnet(subnet).await,
            Self::Postgres(s) => s.insert_subnet(subnet).await,
        }
    }

    async fn list_subnets(&self, project_id: &str) -> Result<Vec<SubnetRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_subnets(project_id).await,
            Self::Postgres(s) => s.list_subnets(project_id).await,
        }
    }

    async fn list_subnets_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<SubnetRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_subnets_for_network(project_id, network_id).await,
            Self::Postgres(s) => s.list_subnets_for_network(project_id, network_id).await,
        }
    }

    async fn get_subnet(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SubnetRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_subnet(project_id, id).await,
            Self::Postgres(s) => s.get_subnet(project_id, id).await,
        }
    }

    async fn delete_subnet(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_subnet(project_id, id).await,
            Self::Postgres(s) => s.delete_subnet(project_id, id).await,
        }
    }

    async fn insert_port(&self, port: &PortRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_port(port).await,
            Self::Postgres(s) => s.insert_port(port).await,
        }
    }

    async fn list_ports(&self, project_id: &str) -> Result<Vec<PortRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_ports(project_id).await,
            Self::Postgres(s) => s.list_ports(project_id).await,
        }
    }

    async fn list_ports_for_network(
        &self,
        project_id: &str,
        network_id: &Uuid,
    ) -> Result<Vec<PortRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_ports_for_network(project_id, network_id).await,
            Self::Postgres(s) => s.list_ports_for_network(project_id, network_id).await,
        }
    }

    async fn get_port(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<PortRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_port(project_id, id).await,
            Self::Postgres(s) => s.get_port(project_id, id).await,
        }
    }

    async fn delete_port(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_port(project_id, id).await,
            Self::Postgres(s) => s.delete_port(project_id, id).await,
        }
    }

    async fn update_port_binding(
        &self,
        project_id: &str,
        id: &Uuid,
        binding_host: Option<&str>,
        binding_state: Option<&str>,
    ) -> Result<PortRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_port_binding(project_id, id, binding_host, binding_state)
                    .await
            }
            Self::Postgres(s) => {
                s.update_port_binding(project_id, id, binding_host, binding_state)
                    .await
            }
        }
    }

    async fn insert_security_group(&self, group: &SecurityGroupRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_security_group(group).await,
            Self::Postgres(s) => s.insert_security_group(group).await,
        }
    }
    async fn list_security_groups(
        &self,
        project_id: &str,
    ) -> Result<Vec<SecurityGroupRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_security_groups(project_id).await,
            Self::Postgres(s) => s.list_security_groups(project_id).await,
        }
    }
    async fn get_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_security_group(project_id, id).await,
            Self::Postgres(s) => s.get_security_group(project_id, id).await,
        }
    }
    async fn update_security_group(
        &self,
        project_id: &str,
        id: &Uuid,
        name: &str,
        description: &str,
    ) -> Result<SecurityGroupRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.update_security_group(project_id, id, name, description)
                    .await
            }
            Self::Postgres(s) => {
                s.update_security_group(project_id, id, name, description)
                    .await
            }
        }
    }
    async fn delete_security_group(&self, project_id: &str, id: &Uuid) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_security_group(project_id, id).await,
            Self::Postgres(s) => s.delete_security_group(project_id, id).await,
        }
    }
    async fn insert_security_group_rule(
        &self,
        rule: &SecurityGroupRuleRecord,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.insert_security_group_rule(rule).await,
            Self::Postgres(s) => s.insert_security_group_rule(rule).await,
        }
    }
    async fn list_security_group_rules(
        &self,
        project_id: &str,
        group_id: &Uuid,
    ) -> Result<Vec<SecurityGroupRuleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_security_group_rules(project_id, group_id).await,
            Self::Postgres(s) => s.list_security_group_rules(project_id, group_id).await,
        }
    }
    async fn get_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<Option<SecurityGroupRuleRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_security_group_rule(project_id, id).await,
            Self::Postgres(s) => s.get_security_group_rule(project_id, id).await,
        }
    }
    async fn delete_security_group_rule(
        &self,
        project_id: &str,
        id: &Uuid,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_security_group_rule(project_id, id).await,
            Self::Postgres(s) => s.delete_security_group_rule(project_id, id).await,
        }
    }
    async fn list_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: Option<&Uuid>,
    ) -> Result<Vec<SecurityGroupBindingRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.list_security_group_bindings(project_id, endpoint_id)
                    .await
            }
            Self::Postgres(s) => {
                s.list_security_group_bindings(project_id, endpoint_id)
                    .await
            }
        }
    }
    async fn replace_security_group_bindings(
        &self,
        project_id: &str,
        endpoint_id: &Uuid,
        group_ids: &[Uuid],
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.replace_security_group_bindings(project_id, endpoint_id, group_ids)
                    .await
            }
            Self::Postgres(s) => {
                s.replace_security_group_bindings(project_id, endpoint_id, group_ids)
                    .await
            }
        }
    }
}

#[async_trait]
impl PlacementRepository for O3kStore {
    async fn get_provider(
        &self,
        provider_id: &str,
    ) -> Result<Option<PlacementProviderRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_provider(provider_id).await,
            Self::Postgres(s) => s.get_provider(provider_id).await,
        }
    }

    async fn list_providers(&self) -> Result<Vec<PlacementProviderRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_providers().await,
            Self::Postgres(s) => s.list_providers().await,
        }
    }

    async fn register_provider(
        &self,
        node_id: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.register_provider(node_id, inventories).await,
            Self::Postgres(s) => s.register_provider(node_id, inventories).await,
        }
    }

    async fn sync_provider(
        &self,
        node_id: &str,
        state: &str,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.sync_provider(node_id, state, inventories).await,
            Self::Postgres(s) => s.sync_provider(node_id, state, inventories).await,
        }
    }

    async fn refresh_inventories(
        &self,
        provider_id: &str,
        expected_generation: u64,
        inventories: &[PlacementInventoryRecord],
    ) -> Result<PlacementProviderRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.refresh_inventories(provider_id, expected_generation, inventories)
                    .await
            }
            Self::Postgres(s) => {
                s.refresh_inventories(provider_id, expected_generation, inventories)
                    .await
            }
        }
    }

    async fn set_provider_state(&self, provider_id: &str, state: &str) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.set_provider_state(provider_id, state).await,
            Self::Postgres(s) => s.set_provider_state(provider_id, state).await,
        }
    }

    async fn commit_allocation(
        &self,
        provider_id: &str,
        expected_generation: u64,
        allocation: &PlacementAllocationRecord,
    ) -> Result<PlacementAllocationRecord, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.commit_allocation(provider_id, expected_generation, allocation)
                    .await
            }
            Self::Postgres(s) => {
                s.commit_allocation(provider_id, expected_generation, allocation)
                    .await
            }
        }
    }

    async fn release_allocation(
        &self,
        provider_id: &str,
        allocation_id: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_allocation(provider_id, allocation_id).await,
            Self::Postgres(s) => s.release_allocation(provider_id, allocation_id).await,
        }
    }

    async fn upsert_intent(
        &self,
        intent: &PlacementIntentRecord,
    ) -> Result<PlacementIntentRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.upsert_intent(intent).await,
            Self::Postgres(s) => s.upsert_intent(intent).await,
        }
    }

    async fn get_intent(
        &self,
        allocation_id: &str,
    ) -> Result<Option<PlacementIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_intent(allocation_id).await,
            Self::Postgres(s) => s.get_intent(allocation_id).await,
        }
    }

    async fn list_intents(&self) -> Result<Vec<PlacementIntentRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_intents().await,
            Self::Postgres(s) => s.list_intents().await,
        }
    }

    async fn delete_intent(&self, allocation_id: &str) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.delete_intent(allocation_id).await,
            Self::Postgres(s) => s.delete_intent(allocation_id).await,
        }
    }

    async fn reconcile_consumers(
        &self,
        durable_consumer_ids: &[String],
    ) -> Result<PlacementReconcileRecord, StoreError> {
        match self {
            Self::Sqlite(s) => s.reconcile_consumers(durable_consumer_ids).await,
            Self::Postgres(s) => s.reconcile_consumers(durable_consumer_ids).await,
        }
    }

    async fn import_provider(&self, provider: &PlacementProviderRecord) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.import_provider(provider).await,
            Self::Postgres(s) => s.import_provider(provider).await,
        }
    }
}

#[async_trait]
impl QuotaRepository for O3kStore {
    async fn get_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
    ) -> Result<LimitValue, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_limit(scope, key).await,
            Self::Postgres(s) => s.get_limit(scope, key).await,
        }
    }

    async fn set_limit(
        &self,
        scope: &OwnershipScope,
        key: &LimitKey,
        limit: LimitValue,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.set_limit(scope, key, limit).await,
            Self::Postgres(s) => s.set_limit(scope, key, limit).await,
        }
    }

    async fn get_usage(&self, scope: &OwnershipScope, key: &LimitKey) -> Result<Usage, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_usage(scope, key).await,
            Self::Postgres(s) => s.get_usage(scope, key).await,
        }
    }

    async fn reserve_quota(
        &self,
        scope: &OwnershipScope,
        operation_id: &str,
        amounts: &[ResourceAmount],
    ) -> Result<Reservation, StoreError> {
        match self {
            Self::Sqlite(s) => s.reserve_quota(scope, operation_id, amounts).await,
            Self::Postgres(s) => s.reserve_quota(scope, operation_id, amounts).await,
        }
    }

    async fn commit_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.commit_reservation(reservation_id).await,
            Self::Postgres(s) => s.commit_reservation(reservation_id).await,
        }
    }

    async fn release_reservation(&self, reservation_id: &ReservationId) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_reservation(reservation_id).await,
            Self::Postgres(s) => s.release_reservation(reservation_id).await,
        }
    }

    async fn release_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.release_reservation_for_operation(operation_id).await,
            Self::Postgres(s) => s.release_reservation_for_operation(operation_id).await,
        }
    }

    async fn get_reservation_for_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<Reservation>, StoreError> {
        match self {
            Self::Sqlite(s) => s.get_reservation_for_operation(operation_id).await,
            Self::Postgres(s) => s.get_reservation_for_operation(operation_id).await,
        }
    }
}

#[async_trait]
impl ComputeRepository for O3kStore {
    async fn list_resources_by_kind(&self, kind: &str) -> Result<Vec<ResourceRecord>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_resources_by_kind(kind).await,
            Self::Postgres(s) => s.list_resources_by_kind(kind).await,
        }
    }
}

#[async_trait]
impl CoordinationRepository for O3kStore {
    async fn register_controller_session(
        &self,
        session: &ControllerSession,
        ttl: std::time::Duration,
    ) -> Result<(), StoreError> {
        match self {
            Self::Sqlite(s) => s.register_controller_session(session, ttl).await,
            Self::Postgres(s) => s.register_controller_session(session, ttl).await,
        }
    }

    async fn heartbeat_controller_session(
        &self,
        controller_id: &ControllerId,
        controller_epoch: &ControllerEpoch,
        ttl: std::time::Duration,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.heartbeat_controller_session(controller_id, controller_epoch, ttl)
                    .await
            }
            Self::Postgres(s) => {
                s.heartbeat_controller_session(controller_id, controller_epoch, ttl)
                    .await
            }
        }
    }

    async fn drain_controller_session(
        &self,
        controller_id: &ControllerId,
        controller_epoch: &ControllerEpoch,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.drain_controller_session(controller_id, controller_epoch)
                    .await
            }
            Self::Postgres(s) => {
                s.drain_controller_session(controller_id, controller_epoch)
                    .await
            }
        }
    }

    async fn acquire_work_lease_once(
        &self,
        work_key: &str,
        work_kind: &str,
        controller_id: &ControllerId,
        controller_epoch: &ControllerEpoch,
        ttl: std::time::Duration,
    ) -> Result<LeaseAcquireOutcome, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.acquire_work_lease(work_key, work_kind, controller_id, controller_epoch, ttl)
                    .await
            }
            Self::Postgres(s) => {
                s.acquire_work_lease(work_key, work_kind, controller_id, controller_epoch, ttl)
                    .await
            }
        }
    }

    async fn renew_work_lease(
        &self,
        work_key: &str,
        controller_id: &ControllerId,
        controller_epoch: &ControllerEpoch,
        fencing_token: FencingToken,
        ttl: std::time::Duration,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.renew_work_lease(
                    work_key,
                    controller_id,
                    controller_epoch,
                    fencing_token,
                    ttl,
                )
                .await
            }
            Self::Postgres(s) => {
                s.renew_work_lease(
                    work_key,
                    controller_id,
                    controller_epoch,
                    fencing_token,
                    ttl,
                )
                .await
            }
        }
    }

    async fn release_work_lease(
        &self,
        work_key: &str,
        controller_id: &ControllerId,
        controller_epoch: &ControllerEpoch,
        fencing_token: FencingToken,
    ) -> Result<bool, StoreError> {
        match self {
            Self::Sqlite(s) => {
                s.release_work_lease(work_key, controller_id, controller_epoch, fencing_token)
                    .await
            }
            Self::Postgres(s) => {
                s.release_work_lease(work_key, controller_id, controller_epoch, fencing_token)
                    .await
            }
        }
    }

    async fn inspect_work_lease(&self, work_key: &str) -> Result<Option<WorkLease>, StoreError> {
        match self {
            Self::Sqlite(s) => s.inspect_work_lease(work_key).await,
            Self::Postgres(s) => s.inspect_work_lease(work_key).await,
        }
    }

    async fn list_active_controller_sessions(&self) -> Result<Vec<ControllerSession>, StoreError> {
        match self {
            Self::Sqlite(s) => s.list_active_controller_sessions().await,
            Self::Postgres(s) => s.list_active_controller_sessions().await,
        }
    }
}
