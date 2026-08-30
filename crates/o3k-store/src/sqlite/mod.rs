//! SQLite persistence adapter for the O3K store.

//! This module owns the stable SqliteStore facade and assembles the
//! responsibility-oriented SQLite implementation modules.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::Utc;
use md5::{Digest as Md5Digest, Md5};
use sqlx::{
    Row, SqlitePool,
    sqlite::{
        SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous,
    },
};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::{fs, net::Ipv4Addr, path::Path, str::FromStr, sync::Arc, time::Duration};
use uuid::Uuid;

use crate::{
    AgentCommandRecord, AgentCommandState, ArtifactTransferRecord, ArtifactTransferUpdate,
    CanonicalAcceptanceOutcome, CanonicalAddressPoolRecord, CanonicalAddressRealmRecord,
    CanonicalEndpointRecord, CanonicalL3GatewayAttachmentRecord, CanonicalL3GatewayRecord,
    CanonicalNetworkPolicyRecord, CanonicalNetworkRecord, CanonicalOperationLifecycleUpdate,
    CanonicalOperationRecord, CanonicalRealmBindingRecord, ComputeRepository, DatabaseHealth,
    DurableStore, IdempotencyReservation, IdempotencyReservationRequest, IdentityRepository,
    ImageMetadataRecord, ImageOverlayIdentity, ImageOverlayOwnershipRecord, ImageOverlayState,
    ImageOverlayUpdate, ImageRepository, KeypairRecord, KeypairRepository, KeystoneDomainRecord,
    KeystoneEndpointRecord, KeystoneProjectRecord, KeystoneRegionRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord, KeystoneUserRecord,
    NetworkAddressAllocationRecord, NetworkIntentRecord, NetworkRecord, NetworkRepository,
    ObservationUpdate, OperationRecord, OperationState, PlacementAllocationRecord,
    PlacementIntentRecord, PlacementInventoryRecord, PlacementProviderRecord,
    PlacementReconcileRecord, PlacementRepository, PlacementResourceRecord, PortRecord,
    ProviderReference, RELATIONSHIP_BOUND, RELATIONSHIP_DELETED, RELATIONSHIP_DELETING,
    RELATIONSHIP_RESERVED, RELATIONSHIP_UNKNOWN, RelationshipRepository, ResourceRecord,
    ResourceRelationshipRecord, SQLITE_BUSY_MAX_ATTEMPTS, SecurityGroupBindingRecord,
    SecurityGroupRecord, SecurityGroupRuleRecord, StoreError, SubnetRecord, VolumeAttachmentRecord,
    VolumeAttachmentRepository, WalCheckpointMode, is_sqlite_busy, legacy_policy_records,
    relationship_from_row, restrict_sqlite_sidecars,
    validate_canonical_idempotent_operation_identity, validate_canonical_lifecycle_update,
    validate_canonical_operation_read, validate_canonical_resource_acceptance,
};

mod core;
mod helpers;
mod identity;
mod image;
mod network;
mod placement;
mod relationship;
mod volume_attachment;

pub use helpers::validate_public_key;
pub(crate) use helpers::*;

#[derive(Clone, Debug)]
pub struct SqliteStore {
    pub(crate) pool: SqlitePool,
    agent_command_projection_lock: Arc<tokio::sync::Mutex<()>>,
}
