#!/usr/bin/env python3
"""Extract o3k-placement/src/lib.rs into modules: types.rs, ledger.rs."""

from pathlib import Path

SRC = Path("crates/o3k-placement/src/lib.rs")
DST = Path("crates/o3k-placement/src")
lines = SRC.read_text().splitlines(keepends=True)

# Line ranges (1-based, inclusive start, exclusive end)
# [1-11]    Imports
# [12-103]  Domain types + constants
# [105-615] PlacementLedger + helpers
# [617-1351] Test module

ranges = {
    "types": (12, 104),  # types + constants
    "ledger": (105, 616),  # PlacementLedger + helpers
}

def extract(name, start, end):
    """Extract lines and apply transforms."""
    raw = "".join(lines[start-1:end-1])
    
    if name == "types":
        raw = """\
//! Domain types for placement: providers, inventories, allocations.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;

""" + raw
    elif name == "ledger":
        raw = """\
//! Placement ledger — inventory and allocation management.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::{Deserialize, Serialize};

use crate::types::{
    Allocation, AllocationIntent, Inventory, OrphanedAllocation, PlacementError, ProviderState,
    ReconciliationReport, ResourceProvider, VCPU, MEMORY_MB, DISK_GB,
};

""" + raw
        # Make private functions pub(crate) so tests in lib.rs can access them
        for fn_name in [
            "map_store_error", "provider_state_as_str", "provider_state_from_str",
            "inventory_records", "inventory_map", "resource_records", "resource_map",
            "provider_from_record", "allocation_from_record", "intent_from_record",
            "provider_to_record", "intent_to_record",
        ]:
            old = f"\nfn {fn_name}"
            new = f"\npub(crate) fn {fn_name}"
            raw = raw.replace(old, new)

    (DST / f"{name}.rs").write_text(raw)
    print(f"  {name}.rs: {end-start} lines")

# Extract modules
print("Extracting placement modules...")
for name, (start, end) in ranges.items():
    extract(name, start, end)

# Write new lib.rs
lib_content = """\
//! Small Placement-compatible inventory and allocation ledger.

pub mod types;
pub mod ledger;

pub use ledger::PlacementLedger;
pub use types::{
    Allocation, AllocationIntent, Inventory, OrphanedAllocation, PlacementError, ProviderState,
    ReconciliationReport, ResourceProvider, VCPU, MEMORY_MB, DISK_GB,
};

"""

# Test module (lines 617-1351)
test_raw = "".join(lines[616:])  # 617 1-based -> 616 0-based
lib_content += test_raw

(DST / "lib.rs").write_text(lib_content)
print(f"  lib.rs: rewritten as re-export hub (+ {len(test_raw.splitlines())} test lines)")

print("\nDone.")
for p in sorted(DST.glob("*.rs")):
    print(f"  {p.name}: {len(p.read_text().splitlines())} lines")
