#!/usr/bin/env bash
# Permanent regression test for the final architecture guard.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUARD="$REPO_ROOT/scripts/check-maintainability-guards.py"
PASS=0
FAIL=0
ERRORS=""
CLEANUP_FILES=()

cleanup() {
    for file in "${CLEANUP_FILES[@]}"; do
        rm -f -- "$file"
    done
    rmdir "$REPO_ROOT/tests/z_guard_fixtures" 2>/dev/null || true
}
trap cleanup EXIT

check_fixture() {
    local name="$1" expected_exit="$2" expected_pattern="$3"
    local fixture="$4" fixture_dir="$5"
    local fixture_path="$REPO_ROOT/$fixture_dir/z_guard_test_fixture.rs"

    printf '%s\n' "$fixture" > "$fixture_path"
    CLEANUP_FILES+=("$fixture_path")
    local rc=0 output
    output=$(python3 "$GUARD" 2>&1) || rc=$?

    if [ "$expected_exit" -eq 0 ] && [ "$rc" -eq 0 ]; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    elif [ "$expected_exit" -ne 0 ] && [ "$rc" -ne 0 ] && \
         echo "$output" | grep -qF "$expected_pattern"; then
        echo "  PASS: $name"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: $name (expected exit $expected_exit, got $rc)"
        echo "    guard output: $(echo "$output" | tail -10)"
        FAIL=$((FAIL + 1))
        ERRORS+="  - $name: expected exit $expected_exit, got $rc\n"
    fi
    rm -f -- "$fixture_path"
}

echo "=== Maintainability Guard Regression Suite ==="
echo
echo "Test 1: clean repository and approved boundaries -> PASS"
rc=0
output=$(python3 "$GUARD" 2>&1) || rc=$?
if [ "$rc" -eq 0 ]; then
    echo "  PASS: clean repository"
    PASS=$((PASS + 1))
else
    echo "  FAIL: clean repository (got $rc)"
    echo "    guard output: $(echo "$output" | tail -10)"
    FAIL=$((FAIL + 1))
    ERRORS+="  - clean repository: expected exit 0, got $rc\n"
fi

echo "Test 2: SQL in store/domain -> FAIL"
check_fixture "SQL in store/domain" 1 "SQL ARCHITECTURE VIOLATION" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }' "crates/o3k-store/src/domain"
echo "Test 3: SQL in store/port -> FAIL"
check_fixture "SQL in store/port" 1 "SQL ARCHITECTURE VIOLATION" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }' "crates/o3k-store/src/port"
echo "Test 4: SQL in store/unified -> FAIL"
check_fixture "SQL in store/unified" 1 "SQL ARCHITECTURE VIOLATION" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }' "crates/o3k-store/src/unified"
echo "Test 5: SQL in o3kd composition -> FAIL"
check_fixture "SQL in o3kd composition" 1 "SQL ARCHITECTURE VIOLATION" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }' "bins/o3kd/src/composition"
echo "Test 6: SQL in Network application -> FAIL"
check_fixture "SQL in Network application" 1 "SQL ARCHITECTURE VIOLATION" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }' "crates/o3k-network/src"

echo "Test 7: imported bare Command::new -> FAIL"
check_fixture "bare Command::new" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    $'use std::process::Command;\npub fn z_run() { let _x = Command::new("ls"); }' "crates/o3k-kernel/src"
echo "Test 8: fully qualified Command::new -> FAIL"
check_fixture "fully qualified Command::new" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { let _x = std::process::Command::new("ls"); }' "crates/o3k-kernel/src"
echo "Test 9: tokio qualified Command::new -> FAIL"
check_fixture "tokio qualified Command::new" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { let _x = tokio::process::Command::new("ls"); }' "crates/o3k-kernel/src"
echo "Test 10: Command::new after #[cfg(test)] -> FAIL"
check_fixture "Command after cfg(test)" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    $'#[cfg(test)] mod tests {}\nfn production_after_tests() { let _ = Command::new("ls"); }' "crates/o3k-kernel/src"
echo "Test 11: SQL after #[cfg(test)] -> FAIL"
check_fixture "SQL after cfg(test)" 1 "SQL ARCHITECTURE VIOLATION" \
    $'#[cfg(test)] mod tests {}\nfn production_after_tests() { sqlx::query("SELECT 1"); }' "crates/o3k-kernel/src"

echo "Test 12: raw Linux wrapper in canonical Network -> FAIL"
check_fixture "raw Linux wrapper in canonical Network" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { run("ip", &["link"]); }' "crates/o3k-network/src"
echo "Test 13: host command in o3kd composition -> FAIL"
check_fixture "host command in o3kd composition" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { let _ = Command::new("ip"); }' "bins/o3kd/src/composition"
echo "Test 14: host command in Compute binary source -> FAIL"
check_fixture "host command in Compute binary source" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { let _ = Command::new("ip"); }' "bins/o3k-compute/src"
echo "Test 15: shell execution -> FAIL"
check_fixture "shell execution" 1 "HOST EXECUTION ARCHITECTURE VIOLATION" \
    'pub fn z_run() { let _ = Command::new("sh").arg("-c").arg("true"); }' "crates/o3k-kernel/src"

echo "Test 16: test-only file -> ACCEPT"
TEST_FIXTURE_DIR="$REPO_ROOT/tests/z_guard_fixtures"
mkdir -p "$TEST_FIXTURE_DIR"
TEST_FIXTURE="$TEST_FIXTURE_DIR/test_guard_ok.rs"
CLEANUP_FILES+=("$TEST_FIXTURE")
printf '%s\n' 'pub fn z_query() { sqlx::query("SELECT 1"); }' > "$TEST_FIXTURE"
rc=0
output=$(python3 "$GUARD" 2>&1) || rc=$?
if [ "$rc" -eq 0 ]; then
    echo "  PASS: test-only file ignored"
    PASS=$((PASS + 1))
else
    echo "  FAIL: test-only file caused violation"
    echo "    guard output: $(echo "$output" | tail -5)"
    FAIL=$((FAIL + 1))
    ERRORS+="  - test-only file: unexpected violation\n"
fi
rm -f -- "$TEST_FIXTURE"
rmdir "$TEST_FIXTURE_DIR" 2>/dev/null || true

echo "Test 17: dependency cycle -> SKIP (requires isolated Cargo.toml fixture)"
echo "Test 18: weakened safety policy -> SKIP (requires isolated Cargo.toml fixture)"
echo
echo "=== Results ==="
echo "  PASS: $PASS"
echo "  FAIL: $FAIL"
if [ -n "$ERRORS" ]; then
    echo "  Errors:"
    echo -e "$ERRORS"
    exit 1
fi
echo "  All regression tests passed."
