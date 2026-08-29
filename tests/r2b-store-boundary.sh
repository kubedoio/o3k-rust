#!/usr/bin/env bash
set -euo pipefail

store_src="crates/o3k-store/src"
sql_pattern='sqlx::query(_as|_scalar)?|\.begin\(\)|FOR UPDATE|ON CONFLICT|RETURNING'

fail() {
    printf 'r2b store boundary: %s\n' "$1" >&2
    exit 1
}

[[ ! -e "$store_src/postgres.rs" ]] || fail "monolithic postgres.rs remains"
[[ ! -e "$store_src/unified.rs" ]] || fail "monolithic unified.rs remains"

if rg -n "$sql_pattern" "$store_src/domain" "$store_src/port" "$store_src/unified"; then
    fail "SQL or transaction SQL leaked into domain, port, or unified modules"
fi

postgres_hits=$(rg -l "$sql_pattern" "$store_src" --glob '*.rs' || true)
while IFS= read -r path; do
    [[ -z "$path" ]] && continue
    case "$path" in
        "$store_src/postgres/"*) ;;
        "$store_src/sqlite/"*) ;;
        "$store_src/artifact_transfer.rs"|\
        "$store_src/coordination.rs"|\
        "$store_src/quota.rs"|\
        "$store_src/reusable_policy.rs"|\
        "$store_src/server_state.rs"|\
        "$store_src/storage.rs"|\
        "$store_src/conformance.rs"|\
        "$store_src/tests.rs") ;;
        *) fail "SQL is outside an implementation module: $path" ;;
    esac
done <<< "$postgres_hits"

[[ "$(rg -l '^pub struct PostgresStore' "$store_src/postgres" | sort)" == "$store_src/postgres/mod.rs" ]] ||
    fail "PostgresStore facade is not defined only in postgres/mod.rs"
[[ "$(rg -l '^pub enum O3kStore' "$store_src/unified" | sort)" == "$store_src/unified/mod.rs" ]] ||
    fail "O3kStore facade is not defined only in unified/mod.rs"

printf 'r2b store boundary: PASS\n'
