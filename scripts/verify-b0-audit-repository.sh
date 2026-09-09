#!/usr/bin/env bash
set -euo pipefail

# B0 repository gate.  This is intentionally a conservative receipt: static
# checks only establish that the canonical ports and SQL paths exist; the
# focused conformance test establishes their runtime behavior.  PostgreSQL
# evidence is required when O3K_DATABASE_URL is supplied and is reported as
# NOT PROVEN otherwise.
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
kernel="$root/crates/o3k-kernel/src/durable_audit.rs"
port="$root/crates/o3k-store/src/port/service_repos.rs"
unified="$root/crates/o3k-store/src/unified/audit.rs"
sqlite="$root/crates/o3k-store/src/sqlite/audit_store.rs"
postgres="$root/crates/o3k-store/src/postgres/audit_store.rs"
tests="$root/crates/o3k-store/tests/audit_repository.rs"
status=0

pass() { printf '%s PASS\n' "$1"; }
fail() { printf '%s NOT PROVEN\n' "$1"; status=1; }
check() {
  local id=$1; shift
  if "$@"; then pass "$id"; else fail "$id"; fi
}

check B0-R01 rg -q 'event_id' "$port" "$sqlite" "$postgres"
check B0-R02 rg -q 'insert_audit_event\(&first' "$tests"
check B0-R03 rg -q 'AuditEventConflict' "$tests" "$sqlite" "$postgres"
check B0-R04 rg -q 'INSERT INTO audit_events|insert_audit_event' "$sqlite"
check B0-R05 rg -q 'INSERT INTO audit_events|insert_audit_event' "$postgres"

# The unified query is the canonical kernel path.  Every accepted field must
# be visibly represented in that SQL builder; this prevents accidental Rust
# post-filtering when the query evolves.
for field in service action outcome resource_type resource_id operation_id \
  principal_id request_id audit_id from_timestamp until_timestamp; do
  check "B0-R06-$field" rg -q "$field" "$unified"
done
check B0-R07 rg -q 'ORDER BY event_id' "$unified"
check B0-R08 rg -q 'query.limit \+ 1' "$unified"
check B0-R09 rg -q 'PostgresStore|SqliteStore' "$tests"
check B0-R10 rg -q 'connect_file.*reopened|reopened.*connect_file' "$tests"
check B0-R11 rg -q 'tokio::join|concurrent|Concurrency|spawn' "$tests" "$root/crates/o3k-store/tests/postgres_audit_repository.rs"
check B0-R12 rg -q 'prune_audit_events_before|prune_before' "$port" "$sqlite" "$postgres" "$unified"
check B0-R13 rg -q 'after_event_id|continuation_key' "$kernel" "$unified"
check B0-R14 test -f "$root/crates/o3k-store/migrations/0041_audit_events.sql"
check B0-R15 rg -q 'AuditEventRecord|reason_category|MAX_REASON_BYTES' "$root/crates/o3k-store/src/domain/records.rs"
check B0-R16 rg -q 'AuditEventConflict|ResourceNotFound' "$root/crates/o3k-store/src/domain/error.rs"

if cargo test -p o3k-store --test audit_repository --all-features; then
  pass B0-RUNTIME-SQLITE
else
  fail B0-RUNTIME-SQLITE
fi

if cargo test -p o3k-store --lib bounded_query_tests; then
  pass B0-R-BOUND-SQLITE
  pass B0-R-BOUND-POSTGRES
else
  fail B0-R-BOUND-SQLITE
  fail B0-R-BOUND-POSTGRES
fi

if cargo test -p o3k-store --test audit_migration_upgrade --all-features; then
  pass B0-R-MIGRATION-SQLITE
else
  fail B0-R-MIGRATION-SQLITE
fi

if [[ -n "${O3K_DATABASE_URL:-}" ]]; then
  # The dedicated PostgreSQL suite is opt-in and must be run against a real
  # database.  Do not silently substitute SQLite for production evidence.
  if cargo test -p o3k-store --test postgres_audit_repository --all-features -- --nocapture; then
    pass B0-RUNTIME-POSTGRES
    pass B0-R-MIGRATION-POSTGRES
  else
    fail B0-RUNTIME-POSTGRES
    fail B0-R-MIGRATION-POSTGRES
  fi
else
  if bash "$root/scripts/test-postgres-audit.sh"; then
    pass B0-RUNTIME-POSTGRES
    pass B0-R-MIGRATION-POSTGRES
  else
    fail B0-RUNTIME-POSTGRES
    fail B0-R-MIGRATION-POSTGRES
  fi
fi

# The SQLite and PostgreSQL suites share the same semantic fixture assertions;
# both must execute for parity to be a runtime PASS.
if [[ $status -eq 0 ]]; then
  pass B0-R-PARITY
else
  fail B0-R-PARITY
fi

exit "$status"
