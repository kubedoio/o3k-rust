//! Unified O3K store abstraction supporting both SQLite and PostgreSQL backends.

use async_trait::async_trait;
use std::path::Path;
use uuid::Uuid;

use o3k_kernel::{
    LimitKey, LimitValue, OwnershipScope, Reservation, ReservationId, ResourceAmount, Usage,
};

use crate::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferUpdate,
    CanonicalAddressPoolRecord, CanonicalAddressRealmRecord, CanonicalEndpointRecord,
    CanonicalL3GatewayAttachmentRecord, CanonicalL3GatewayRecord, CanonicalNetworkPolicyRecord,
    CanonicalNetworkPolicyRuleRecord, CanonicalNetworkRecord, CanonicalOperationRecord,
    CanonicalPolicyAttachmentRecord, CanonicalPolicyRealizationRecord, CanonicalPolicyRepository,
    CanonicalRealmBindingRecord, CanonicalReusableNetworkPolicyRecord, ComputeRepository,
    ControllerEpoch, ControllerId, ControllerSession, CoordinationRepository, DatabaseHealth,
    DurableStore, FencingToken, IdempotencyReservation, IdempotencyReservationRequest,
    IdentityRepository, ImageMetadataRecord, ImageOverlayIdentity, ImageOverlayOwnershipRecord,
    ImageOverlayUpdate, ImageRepository, KeypairRecord, KeypairRepository, KeystoneDomainRecord,
    KeystoneEndpointRecord, KeystoneProjectRecord, KeystoneRegionRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord, KeystoneUserRecord,
    LeaseAcquireOutcome, NetworkAddressAllocationRecord, NetworkIntentRecord, NetworkRecord,
    NetworkRepository, ObservationUpdate, OperationRecord, OperationState,
    PlacementAllocationRecord, PlacementIntentRecord, PlacementInventoryRecord,
    PlacementProviderRecord, PlacementReconcileRecord, PlacementRepository, PortRecord,
    PostgresStore, ProviderReference, RelationshipRepository, ResourceRecord,
    ResourceRelationshipRecord, SecurityGroupBindingRecord, SecurityGroupRecord,
    SecurityGroupRuleRecord, SnapshotRecord, SqliteStore, StorageBackendRecord, StorageRepository,
    StoreError, SubnetRecord, VolumeAttachmentRecord, VolumeAttachmentRecordV1,
    VolumeAttachmentRepository, VolumeRecord, WorkLease, quota::QuotaRepository,
};

mod compute;
mod coordination;
mod core;
mod identity;
mod image;
mod network;
mod placement;
mod policy;
mod quota;
mod relationship;
mod storage;
mod volume_attachment;

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

    pub async fn reserve_relationship(
        &self,
        record: &ResourceRelationshipRecord,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.reserve_relationship(record).await,
            Self::Postgres(store) => store.reserve_relationship(record).await,
        }
    }

    pub async fn get_relationship(
        &self,
        parent: Uuid,
        slot: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.get_relationship(parent, slot).await,
            Self::Postgres(store) => store.get_relationship(parent, slot).await,
        }
    }

    pub async fn list_relationships(
        &self,
        parent: Uuid,
    ) -> Result<Vec<ResourceRelationshipRecord>, StoreError> {
        match self {
            Self::Sqlite(store) => store.list_relationships(parent).await,
            Self::Postgres(store) => store.list_relationships(parent).await,
        }
    }

    pub async fn bind_relationship(
        &self,
        parent: Uuid,
        slot: &str,
        child: Uuid,
        child_operation: Uuid,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(store) => {
                store
                    .bind_relationship(parent, slot, child, child_operation)
                    .await
            }
            Self::Postgres(store) => {
                store
                    .bind_relationship(parent, slot, child, child_operation)
                    .await
            }
        }
    }

    pub async fn set_relationship_state(
        &self,
        parent: Uuid,
        slot: &str,
        state: &str,
    ) -> Result<ResourceRelationshipRecord, StoreError> {
        match self {
            Self::Sqlite(store) => store.set_relationship_state(parent, slot, state).await,
            Self::Postgres(store) => store.set_relationship_state(parent, slot, state).await,
        }
    }
}
