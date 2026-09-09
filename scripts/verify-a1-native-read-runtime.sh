#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
status=0
check() { local id=$1; shift; if "$@"; then printf '%s PASS\n' "$id"; else printf '%s NOT PROVEN\n' "$id"; status=1; fi; }
check A1-01 rg -q 'ResourcePage<RelationshipView>' "$root/crates/o3k-native-api/src/resource.rs"
check A1-02 rg -q 'list_operations_page' "$root/crates/o3k-native-api/src/operation.rs"
check A1-03 rg -q 'relationships' "$root/crates/o3k-native-api/src/resource.rs"
check A1-04 test "$(rg -l 'decode_cursor|encode_cursor' "$root/crates/o3k-native-api/src" | wc -l)" -ge 1
check A1-05 rg -q 'record.project_id != auth.effective_scope' "$root/bins/o3kd/src/native_adapters/resource.rs"
check A1-06 rg -q 'owner_scoped' "$root/bins/o3kd/src/native_adapters/operation.rs"
check A1-07 rg -q 'ScopeKind::Project' "$root/bins/o3kd/src/native_adapters/operation.rs"
check A1-08 rg -q 'Relationships never grant|never grant' "$root/crates/o3k-native-api/src/resource.rs" "$root/docs"
check A1-09 rg -q 'show_operation' "$root/crates/o3k-native-api/src/operation.rs"
check A1-10 rg -q 'list_relationships_page' "$root/crates/o3k-store/src"
check A1-11 rg -q 'list_canonical_operations_page' "$root/crates/o3k-store/src"
check A1-12 rg -q 'reopen|restart' "$root/bins/o3kd/src/native_adapters/operation.rs"
check A1-13 rg -q 'validate_query' "$root/crates/o3k-native-api/src/operation.rs" "$root/crates/o3k-native-api/src/resource.rs"
if cargo test -p o3kd native_adapters::operation::operation_visibility_tests --all-features >/dev/null && cargo test -p o3k-native-api --all-features >/dev/null; then
  printf 'A1-TESTS PASS\n'
else
  printf 'A1-TESTS NOT PROVEN\n'; status=1
fi
exit "$status"
