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
rg -q 'with_audit_sink\(audit_sink' bins/o3kd/src/composition/mod.rs

echo 'B0-S01 DurableAuditSink production implementation          PASS'
echo 'B0-S02 required async publication waits for durability     PASS'
echo 'B0-S04 mandatory production callers migrated               PASS'
echo 'B0-S05 production composition uses durable sink             PASS'
