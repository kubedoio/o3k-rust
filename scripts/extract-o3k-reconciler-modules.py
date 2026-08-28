#!/usr/bin/env python3
"""Extract o3k-reconciler/src/lib.rs: extract types.rs, keep rest in lib.rs."""

from pathlib import Path

SRC = Path("crates/o3k-reconciler/src/lib.rs")
DST = Path("crates/o3k-reconciler/src")
lines = SRC.read_text().splitlines(keepends=True)

# Extract types: lines 47-212 (1-based, exclusive end)
types_start, types_end = 47, 213
types_raw = "".join(lines[types_start-1:types_end-1])

types_raw = """\
//! Reconciler domain types: events, actions, errors.

use std::sync::{Arc, Mutex};

use o3k_domain::ServerState;
use o3k_provider::AgentObservation;
use o3k_store::AgentCommandRecord;
use thiserror::Error;
use uuid::Uuid;

""" + types_raw

(DST / "types.rs").write_text(types_raw)
print(f"  types.rs: {len(types_raw.splitlines())} lines")

# Rewrite lib.rs: add mod + pub use, remove the type definitions
lib_lines = lines[:]
# Remove lines 47-212 (0-indexed: 46-211)
# But we need to keep the import block intact
# Strategy: keep everything, add `pub mod types;` and `pub use types::*;` after imports,
# and remove the old type definitions

# Actually, simpler: write lib.rs as the original minus lines 47-212,
# with added mod/pub use
lib_header = "".join(lines[:46])  # lines 1-46 (imports + helpers up to types)
lib_rest = "".join(lines[212:])   # lines 213-6192 (OperationJournal + helpers + tests)

lib_content = lib_header
lib_content += "\npub mod types;\n"
lib_content += "pub use types::{\n"
lib_content += "    CanonicalMutationContext, JournalEvent, JournalEventKind, LifecycleAction,\n"
lib_content += "    ReconcileError,\n"
lib_content += "};\n\n"
lib_content += lib_rest

(DST / "lib.rs").write_text(lib_content)
print(f"  lib.rs: rewritten with {len(lib_rest.splitlines())} lines (tests: {lines[2576:]})")

print("\nDone.")
for p in sorted(DST.glob("*.rs")):
    print(f"  {p.name}: {len(p.read_text().splitlines())} lines")
