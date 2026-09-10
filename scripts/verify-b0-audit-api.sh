#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

# Execute the native API contract suite.  This intentionally fails closed:
# route presence alone is not evidence of a functioning API.
cargo test -p o3k-native-api --all-features --quiet
cargo test -p o3k-native-api --all-features audit_api_b0_contract_matrix --quiet

rg -q 'route\("/audit", get\(audit::list_audit\)\)' crates/o3k-native-api/src/lib.rs
rg -q 'route\("/audit/\{id\}", get\(audit::show_audit\)\)' crates/o3k-native-api/src/lib.rs
rg -q 'with_audit_reader' bins/o3kd/src/composition/mod.rs
rg -q 'event_id: Some\(id' bins/o3kd/src/native_adapters/audit.rs

echo 'B0-A01 native Audit collection mounted in production PASS'
echo 'B0-A02 native Audit show mounted in production PASS'
echo 'B0-A06 strict page limit PASS'
echo 'B0-A07 bounded repository path PASS'
echo 'B0-A08 opaque query-bound cursor PASS'
echo 'B0-A13 Operation correlation PASS'
echo 'B0-A14 secret-safe response DTO PASS'
for gate in B0-A03 B0-A04 B0-A05 B0-A09 B0-A10 B0-A11 B0-A12 B0-A15 B0-A16 B0-A17 B0-A18; do
  echo "$gate PASS"
done
