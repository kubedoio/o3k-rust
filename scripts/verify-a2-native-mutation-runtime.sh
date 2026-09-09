#!/usr/bin/env bash
set -euo pipefail

# Conservative source/test gate for the A2 native mutation contract.  This
# verifier reports NOT PROVEN when a required contract marker or focused test
# disappears; it does not claim real-provider or production evidence.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
api="$root/crates/o3k-native-api/src"
adapter="$root/bins/o3kd/src/native_adapters"
tests="$adapter/tests.rs"
status=0
check() {
  local id=$1; shift
  if "$@"; then printf '%s PASS\n' "$id"; else printf '%s NOT PROVEN\n' "$id"; status=1; fi
}

check A2-01 rg -q 'ValidatedUpdateRequest' "$api/resource.rs"
check A2-02 rg -q 'async fn update' "$adapter/resource.rs"
check A2-03 rg -q 'LifecycleOperation::Update' "$adapter/resource.rs"
check A2-04 rg -q 'expected_generation' "$adapter/resource.rs"
check A2-05 rg -q 'PreconditionConflict' "$adapter/resource.rs"
check A2-06 rg -q 'idempotency_key.ok_or' "$adapter/resource.rs"
check A2-07 rg -q 'IdempotencyReservationRequest' "$adapter/resource.rs"
check A2-08 rg -q 'create_or_replay_canonical_lifecycle_operation' "$adapter/resource.rs"
check A2-09 rg -q 'CanonicalAcceptanceOutcome::Conflict' "$adapter/resource.rs"
check A2-10 rg -q 'ExistingEquivalent' "$adapter/resource.rs"
check A2-11 rg -q 'update_canonical_operation_lifecycle' "$adapter/resource.rs"
check A2-12 rg -q 'canonical.*action|Canonical.*action' "$root/crates/o3k-compute/src/actions.rs"
check A2-13 rg -q 'async fn native_compute_declared_action_is_canonical_and_scoped' "$tests"
check A2-14 rg -q 'native_compute_create_changed_body_conflict' "$tests"
check A2-15 rg -q 'native_compute_idempotency_isolated_between_owner_scopes' "$tests"
check A2-16 rg -q 'action replay response|action replay' "$tests"
check A2-17 rg -q 'StatusCode::CONFLICT' "$tests"
check A2-18 rg -q 'StatusCode::NOT_FOUND' "$tests"
check A2-19 rg -q 'StatusCode::NOT_IMPLEMENTED' "$tests"
check A2-20 rg -q 'StatusCode::BAD_REQUEST' "$tests"
check A2-21 rg -q 'provider\.instance_count\(\)' "$tests"
check A2-SCHEMA test -s "$root/contracts/native-action-input-v1.schema.json"
check A2-SCHEMA-REF rg -q 'native-action-input/v1' "$root/crates/o3k-native-api/src/lib.rs"
check A2-OUTPUT-SCHEMA rg -q 'native-mutation-result-v1.schema.json' "$root/crates/o3k-native-api/src/lib.rs"

if cargo test -p o3kd native_adapters::tests::native_compute --all-features >/dev/null; then
  printf 'A2-TESTS PASS\n'
else
  printf 'A2-TESTS NOT PROVEN\n'; status=1
fi
exit "$status"
