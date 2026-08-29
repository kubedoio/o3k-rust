#!/usr/bin/env bash
# Permanent regression test for maintainability guard scripts.
#
# Tests guard correctness without leaving synthetic violations in the
# repository.  Uses temporary fixture files created and destroyed within
# this script.
#
# Run from the repository root:
#   bash tests/maintainability-guards.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
GUARD="$REPO_ROOT/scripts/check-maintainability-guards.py"
FIXTURE_DIR="$REPO_ROOT/crates/o3k-kernel/src"  # unapproved location, production crate
PASS=0
FAIL=0
ERRORS=""

CLEANUP_FILES=()

cleanup() {
    for f in "${CLEANUP_FILES[@]}"; do
        rm -f "$f"
    done
}
trap cleanup EXIT

check() {
    local name="$1"
    local expected_exit="$2"
    local expected_pattern="$3"
    local fixture="$4"
    shift 4

    # Write fixture file
    echo "$fixture" > "$FIXTURE_DIR/z_guard_test_fixture.rs"
    CLEANUP_FILES+=("$FIXTURE_DIR/z_guard_test_fixture.rs")

    local rc=0
    local output
    output=$(python3 "$GUARD" 2>&1) || rc=$?

    if [ "$expected_exit" -eq 0 ]; then
        if [ "$rc" -eq 0 ]; then
            echo "  PASS: $name"
            PASS=$((PASS + 1))
        else
            echo "  FAIL: $name (expected exit 0, got $rc)"
            echo "    guard output: $(echo "$output" | tail -5)"
            FAIL=$((FAIL + 1))
            ERRORS="$ERRORS  - $name: expected exit 0, got $rc\n"
        fi
    else
        if [ "$rc" -ne 0 ] && echo "$output" | grep -qF "$expected_pattern"; then
            echo "  PASS: $name"
            PASS=$((PASS + 1))
        elif [ "$rc" -ne 0 ]; then
            echo "  FAIL: $name (guard exited $rc but missing pattern '$expected_pattern')"
            echo "    guard output: $(echo "$output" | tail -10)"
            FAIL=$((FAIL + 1))
            ERRORS="$ERRORS  - $name: missing pattern '$expected_pattern'\n"
        else
            echo "  FAIL: $name (guard exited 0, expected rejection)"
            FAIL=$((FAIL + 1))
            ERRORS="$ERRORS  - $name: expected rejection but guard passed\n"
        fi
    fi

    # Clean up fixture immediately so subsequent tests start clean
    rm -f "$FIXTURE_DIR/z_guard_test_fixture.rs"
    CLEANUP_FILES=("${CLEANUP_FILES[@]/$FIXTURE_DIR\/z_guard_test_fixture.rs}")
}

echo "=== Maintainability Guard Regression Suite ==="
echo

# --- Test 1: clean repository ---
echo "Test 1: clean repository -> PASS"
(
    rc=0
    output=$(python3 "$GUARD" 2>&1) || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "  PASS: clean repository"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: clean repository (expected exit 0, got $rc)"
        echo "    guard output: $(echo "$output" | tail -5)"
        FAIL=$((FAIL + 1))
        ERRORS="$ERRORS  - clean repository: expected exit 0, got $rc\n"
    fi
)

# --- Test 2: SQL in unapproved production source -> FAIL ---
echo "Test 2: SQL in unapproved production source -> FAIL"
check "SQL in unapproved location" 1 "NEW SQL call site" \
    'pub fn z_query() { sqlx::query("SELECT 1"); }'

# --- Test 3: imported bare Command::new -> FAIL ---
echo "Test 3: imported bare Command::new -> FAIL"
check "bare Command::new (imported)" 1 "candidate architectural leakage" \
    'use std::process::Command;
pub fn z_run() { let _x = Command::new("ls"); }'

# --- Test 4: fully qualified Command::new -> FAIL ---
echo "Test 4: fully qualified Command::new -> FAIL"
check "fully qualified Command::new" 1 "candidate architectural leakage" \
    'pub fn z_run() { let _x = std::process::Command::new("ls"); }'

# --- Test 5: tokio::process::Command::new -> FAIL ---
echo "Test 5: tokio::process::Command::new -> FAIL"
check "tokio qualified Command::new" 1 "candidate architectural leakage" \
    'pub fn z_run() { let _x = tokio::process::Command::new("ls"); }'

# --- Test 6: prohibited call AFTER #[cfg(test)] use -> FAIL ---
echo "Test 6: prohibited call after #[cfg(test)] use -> FAIL"
check "Command::new after #[cfg(test)] use" 1 "candidate architectural leakage" \
    '#[cfg(test)]
use some_test_helper;

use production_dep;

pub fn z_run() { let _x = std::process::Command::new("ls"); }'

# --- Test 7: SQL after #[cfg(test)] use -> FAIL ---
echo "Test 7: SQL after #[cfg(test)] use -> FAIL"
check "SQL after #[cfg(test)] use" 1 "NEW SQL call site" \
    '#[cfg(test)]
use some_test_helper;

pub fn z_query() { sqlx::query("SELECT 1"); }'

# --- Test 8: test-only file -> NOT a production violation ---
echo "Test 8: test-only file -> passes"
# Use a path that matches is_test_or_example: /tests/ in path
TEST_FIXTURE_DIR="$REPO_ROOT/tests/z_guard_fixtures"
mkdir -p "$TEST_FIXTURE_DIR"
echo 'pub fn z_query() { sqlx::query("SELECT 1"); }' > "$TEST_FIXTURE_DIR/test_guard_ok.rs"
CLEANUP_FILES+=("$TEST_FIXTURE_DIR/test_guard_ok.rs")
(
    rc=0
    output=$(python3 "$GUARD" 2>&1) || rc=$?
    if [ "$rc" -eq 0 ]; then
        echo "  PASS: test-only file ignored"
        PASS=$((PASS + 1))
    else
        echo "  FAIL: test-only file caused violation (expected pass)"
        echo "    guard output: $(echo "$output" | tail -5)"
        FAIL=$((FAIL + 1))
        ERRORS="$ERRORS  - test-only file: unexpected violation\n"
    fi
)
rm -f "$TEST_FIXTURE_DIR/test_guard_ok.rs"
rmdir "$TEST_FIXTURE_DIR" 2>/dev/null || true
CLEANUP_FILES=("${CLEANUP_FILES[@]/$TEST_FIXTURE_DIR\/test_guard_ok.rs}")

# --- Test 9: new dependency cycle -> FAIL (if safe to test) ---
# This test requires mutating workspace Cargo.toml which is not safe in an
# automated regression. Marked as SKIP.
echo "Test 9: new dependency cycle -> FAIL"
echo "  SKIP: requires workspace Cargo.toml mutation (destructive)"

# --- Test 10: safety policy weakened -> FAIL ---
# This test requires modifying workspace Cargo.toml which is not safe.
echo "Test 10: safety policy weakened -> FAIL"
echo "  SKIP: requires workspace Cargo.toml mutation (destructive)"

# --- Summary ---
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
exit 0
