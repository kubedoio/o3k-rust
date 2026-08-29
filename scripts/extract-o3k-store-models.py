#!/usr/bin/env python3
"""Extract o3k-store model types into model/ directory organized by domain."""

import subprocess
from pathlib import Path

# Get from git HEAD
result = subprocess.run(['git', 'show', 'HEAD:crates/o3k-store/src/lib.rs'],
                       capture_output=True, text=True)
lines = result.stdout.splitlines(keepends=True)

# Model type definitions and their line ranges (1-based, inclusive start, exclusive end)
# These are all the pub struct/enum definitions in the model section
model_ranges = {
    "model/identity": [
        (534, 608),  # KeystoneDomainRecord through KeystoneRegionRecord
    ],
    "model/network": [
        (128, 360),  # NetworkRecord through CanonicalPolicyAttachmentRecord
        (454, 464),  # NetworkAddressAllocationRecord
    ],
    "model/placement": [
        (463, 515),  # PlacementInventoryRecord through PlacementReconcileRecord
    ],
    "model/storage": [
        (510, 535),  # VolumeAttachmentRecord
        (1195, 1265), # ImageOverlayIdentity through AgentCommandRecord
    ],
    "model/operation": [
        (611, 735),  # OperationState through CanonicalOperationLifecycleUpdate
        (1073, 1193), # IdempotencyReservationRequest through CanonicalAcceptanceOutcome
    ],
    "model/error": [
        (1319, 1420), # StoreError through ResourceRelationshipRecord
    ],
}

# For each model file, extract the type definitions and create the file
MODEL_DIR = Path('crates/o3k-store/src/model')
MODEL_DIR.mkdir(parents=True, exist_ok=True)

# Import headers for each model file based on what its types need
model_imports = {
    "model/identity": """\
use serde::{Deserialize, Serialize};
use uuid::Uuid;
""",
    "model/network": """\
use std::net::Ipv4Addr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
""",
    "model/placement": """\
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use uuid::Uuid;
""",
    "model/storage": """\
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;
""",
    "model/operation": """\
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::model::placement::PlacementProviderRecord;
""",
    "model/error": """\
use std::io;

use thiserror::Error;
use uuid::Uuid;
""",
}

for model_file, ranges in model_ranges.items():
    content_lines = []
    for start, end in ranges:
        content_lines.extend(lines[start-1:end-1])
    
    raw = "".join(content_lines)
    imports = model_imports.get(model_file, "")
    header = f"//! {model_file.split('/')[-1].capitalize()} domain model types.\n\n"
    full = header + imports + "\n" + raw
    
    path = MODEL_DIR / f"{model_file.split('/')[-1]}.rs"
    path.write_text(full)
    print(f"  {path}: {len(full.splitlines())} lines")

# Create model/mod.rs with re-exports
mod_content = """\
pub mod error;
pub mod identity;
pub mod network;
pub mod operation;
pub mod placement;
pub mod storage;

pub use error::StoreError;
pub use identity::{
    KeystoneDomainRecord, KeystoneEndpointRecord, KeystoneProjectRecord, KeystoneRegionRecord,
    KeystoneRoleAssignmentRecord, KeystoneRoleRecord, KeystoneServiceRecord, KeystoneUserRecord,
};
pub use network::{
    CanonicalAddressPoolRecord, CanonicalAddressRealmRecord, CanonicalEndpointRecord,
    CanonicalL3GatewayAttachmentRecord, CanonicalL3GatewayRecord, CanonicalNetworkPolicyRecord,
    CanonicalNetworkRecord, CanonicalPolicyAttachmentRecord, CanonicalPolicyRealizationRecord,
    CanonicalRealmBindingRecord, CanonicalReusableNetworkPolicyRecord,
    NetworkAddressAllocationRecord, NetworkIntentRecord, NetworkRecord, PortRecord,
    SecurityGroupBindingRecord, SecurityGroupRecord, SecurityGroupRuleRecord, SubnetRecord,
    CanonicalNetworkPolicyRuleRecord,
};
pub use operation::{
    CanonicalAcceptanceOutcome, CanonicalOperationLifecycleUpdate, CanonicalOperationRecord,
    IdempotencyReservation, IdempotencyReservationRequest, OperationRecord, OperationState,
    ProviderReference,
};
pub use placement::{
    PlacementAllocationRecord, PlacementIntentRecord, PlacementInventoryRecord,
    PlacementProviderRecord, PlacementReconcileRecord, PlacementResourceRecord,
};
pub use storage::{
    AgentCommandRecord, AgentCommandState, ImageOverlayIdentity, ImageOverlayOwnershipRecord,
    ImageOverlayState, ImageOverlayUpdate, VolumeAttachmentRecord,
};
"""

(MODEL_DIR / "mod.rs").write_text(mod_content)
print(f"  model/mod.rs: {len(mod_content.splitlines())} lines")
