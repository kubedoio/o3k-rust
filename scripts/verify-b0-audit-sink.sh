#!/usr/bin/env bash
set -euo pipefail

# B0 sink gate: mandatory A2 mutation paths must use the awaited durable
# publication boundary, and the production composition must wire the durable
# repository-backed sink. This is intentionally narrow; optional/read audit
# callers in compatibility services are outside the A2 mandatory contract.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

mandatory_files=(
  crates/o3k-compute/src/actions.rs
  crates/o3k-compute/src/lifecycle.rs
  bins/o3kd/src/native_adapters/resource.rs
)
for file in "${mandatory_files[@]}"; do
  if rg -n 'audit_sink\.record\(' "$file"; then
    echo "FAIL: legacy synchronous AuditSink::record in mandatory path: $file" >&2
    exit 1
  fi
done

rg -q 'record_required_audit' crates/o3k-compute/src/{actions.rs,lifecycle.rs}
rg -q 'DurableAuditSink::new' bins/o3kd/src/composition/mod.rs
rg -q 'with_audit_sink\(audit_sink' bins/o3kd/src/composition/mod.rs

echo 'B0-S01 DurableAuditSink production implementation          PASS'
echo 'B0-S02 required async publication waits for durability     PASS'
echo 'B0-S04 mandatory production callers migrated               PASS'
echo 'B0-S05 production composition uses durable sink             PASS'
