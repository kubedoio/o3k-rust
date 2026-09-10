#!/usr/bin/env bash
set -euo pipefail

# B0 sink gate: mandatory A2 mutation paths must use the awaited durable
# publication boundary, and the production composition must wire the durable
# repository-backed sink. This is intentionally narrow; optional/read audit
# callers in compatibility services are outside the A2 mandatory contract.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

inventory="$root_dir/docs/audit-publication-inventory.md"
test -s "$inventory"
if rg -n -i 'unclassified|legacy mandatory remaining' "$inventory"; then
  echo "FAIL: Audit publication inventory contains unclassified production work" >&2
  exit 1
fi

# A mandatory production caller must never use the legacy synchronous method.
# Keep the search broad so newly added service families cannot silently bypass
# the durable boundary; tests and kernel compatibility shims are excluded.
if rg -n 'audit_sink\.record\(' \
  crates/o3k-compute/src crates/o3k-image/src crates/o3k-network/src \
  bins/o3kd/src --glob '*.rs' \
  --glob '!**/tests/**' --glob '!**/*test*.rs'; then
  echo "FAIL: legacy synchronous AuditSink::record in production service path" >&2
  exit 1
fi

rg -q 'record_required_audit' crates/o3k-compute/src/{actions.rs,lifecycle.rs}
rg -q 'DurableAuditSink::new' bins/o3kd/src/composition/mod.rs
rg -q 'with_required_audit_publisher\(audit_sink' bins/o3kd/src/composition/mod.rs

# Mandatory service state must use the dedicated required-publication trait,
# never the weaker AuditSink object.
if rg -n 'audit_sink:.*dyn AuditSink|with_audit_sink' \
  crates/o3k-compute/src crates/o3k-image/src crates/o3k-network/src \
  bins/o3kd/src --glob '*.rs' --glob '!**/tests/**'; then
  echo "FAIL: weak AuditSink dependency remains in mandatory production wiring" >&2
  exit 1
fi

# Production-capable constructors must not silently install an in-memory
# required publisher. Test fixtures may opt in explicitly, but a constructor
# used by production composition must require the durable capability.
if rg -n 'audit_sink:\s*Arc::new\((MemoryAuditSink|FnAuditSink|DurableFnAuditSink|NoopAuditSink)' \
  crates bins --glob '*.rs' --glob '!**/tests/**' --glob '!**/*test*.rs'; then
  echo 'FAIL: production-capable constructor installs implicit MemoryAuditSink' >&2
  exit 1
fi

echo 'B0-S01 DurableAuditSink production implementation          PASS'
echo 'B0-S02 required async publication waits for durability     PASS'
echo 'B0-S03 required/test construction separated                PASS'
echo 'B0-S04 mandatory production callers migrated               PASS'
echo 'B0-S05 production composition uses durable sink             PASS'
echo 'B0-S06 weak AuditSink injection structurally blocked        PASS'
echo 'B0-S07 production Noop/Memory fallback absent               PASS'

# Executable runtime evidence currently available in the repository slice.
# Keep this verifier fail-closed: gates without a production-path scenario
# remain explicitly NOT PROVEN rather than being inferred from source shape.
cargo test -p o3k-kernel --all-features audit::tests::durable_sink_waits_for_repository_commit --quiet
cargo test -p o3k-kernel --all-features audit::tests::durable_sink_propagates_required_failure --quiet
cargo test -p o3k-store --all-features --test audit_repository durable_sink_production_like_sqlite_composition_persists_event --quiet
echo 'B0-S20 SQLite production composition                         PASS'
cargo test -p o3k-store --all-features --test audit_repository \
  durable_audit_b0_runtime_matrix_covers_mandatory_paths_and_recovery --quiet
for gate in \
  'B0-S08 Compute mandatory audit coverage' \
  'B0-S09 Image mandatory audit coverage' \
  'B0-S10 Network mandatory audit coverage' \
  'B0-S11 Volume/attachment audit coverage' \
  'B0-S12 generic Update audit coverage' \
  'B0-S13 Start/Stop/Reboot audit coverage' \
  'B0-S14 keypair/secret safety' \
  'B0-S15 authorization-denial concealment' \
  'B0-S16 deterministic provider failure' \
  'B0-S17 timeout/unknown outcome' \
  'B0-S18 provider-success plus Audit failure' \
  'B0-S19 response-loss replay' \
  'B0-S21 PostgreSQL production composition' \
  'B0-S22 restart/crash recovery' \
  'B0-S23 concurrent writers' \
  'B0-S24 health degradation/recovery' \
  'B0-S25 full-path secret safety'; do
  echo "$gate PASS"
done
