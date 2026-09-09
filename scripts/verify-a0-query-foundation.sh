#!/usr/bin/env bash
set -euo pipefail

# Fail-closed source/evidence gate for A0. This is intentionally conservative:
# source markers alone cannot prove database boundedness, so those rows remain
# NOT PROVEN until the boundedness evidence tests are present and pass.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
native="$root/crates/o3k-native-api/src"
status=0
check() {
  local id=$1; shift
  if "$@"; then printf '%s PASS\n' "$id"; else printf '%s NOT PROVEN\n' "$id"; status=1; fi
}
check_absent() {
  local id=$1; shift
  if ! "$@"; then printf '%s PASS\n' "$id"; else printf '%s NOT PROVEN\n' "$id"; status=1; fi
}

check A0-01 rg -q 'struct ResourceQuery' "$native/pagination.rs"
check A0-02 rg -q 'next_cursor' "$native/pagination.rs"
check A0-03 test "$(rg -l 'bounded_page\s*\(' "$native" | wc -l)" -eq 0
check_absent A0-04 rg -q 'encode_cursor' "$native/resource.rs"
check A0-05 rg -q 'MAX_CURSOR_LENGTH' "$native/pagination.rs"
check A0-06 rg -q 'parse_page_size_strict' "$native/pagination.rs"
check_absent A0-07 rg -q '^\s+async fn list\s*\(' "$native/resource.rs"
check A0-07b rg -q 'list_page' "$native/resource.rs"
check_absent A0-07c rg -q '\.list_resources\s*\(' "$root/bins/o3kd/src/native_adapters/resource.rs"
check_absent A0-07d rg -q '\.list_resources\s*\(' "$root/bins/o3kd/src/native_adapters"
check A0-08 test -s "$root/docs/architecture/a0-native-query-inventory.md"
check A0-09 rg -q 'a0_requested_limit_uses_exactly_one_lookahead' "$root/crates/o3k-store/src"
check A0-10 rg -q 'LIMIT|limit' "$root/crates/o3k-store/src/sqlite"
check A0-11 rg -q 'LIMIT|limit' "$root/crates/o3k-store/src/postgres"
check A0-12 rg -q 'ControllerState::Ready|live.*readiness' "$native/resource.rs"
check A0-13 rg -q 'supports_collection' "$native/resource.rs"
check A0-14 rg -q 'a0_cursor_is_bound_to_scope_resource_and_query_identity' "$native/pagination.rs"
# Mechanical row-fetch and mutation tests must be explicitly named A0; do not
# infer them from ordinary pagination tests.
check A0-15 rg -q 'a0_mutation_between_pages_uses_weak_keyset_semantics' "$native/pagination.rs"

if cargo test -p o3k-native-api --all-features >/dev/null && \
   cargo test -p o3k-store --all-features >/dev/null; then
  printf 'A0-TESTS PASS\n'
else
  printf 'A0-TESTS NOT PROVEN\n'
  status=1
fi
exit "$status"
