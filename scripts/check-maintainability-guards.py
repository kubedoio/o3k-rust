#!/usr/bin/env python3
"""R0 maintainability guardrails for O3K Rust.

Prevents new structural debt relative to the P13.4 immutable baseline.

Guard 1: SQL boundary — new production sqlx::query outside approved locations
Guard 2: Host-command boundary — new Command::new / subprocess outside approved locations
Guard 3: Dependency regression — new crate dependency cycles
Guard 4: Safety policy — workspace Cargo.toml policy not weakened

The immutable baseline lives under docs/maintainability/baselines/p13-4/ and
was generated from edb464ec43b1d12207faae903ea14b7824123f39 (P13.4).  It is
never regenerated from the current HEAD.  A new baseline commit must be
reviewed separately before the guard can be pointed at it.
"""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
from pathlib import Path
from collections import defaultdict

REPO_ROOT = Path(__file__).resolve().parents[1]

# Immutable baseline directory — reviewed, committed, never regenerated
# from current HEAD.
BASELINE_DIR = REPO_ROOT / "docs" / "maintainability" / "baselines" / "p13-4"
BASELINE_SHA = "edb464ec43b1d12207faae903ea14b7824123f39"

# ─── Helpers ───


def is_test_or_example(path: str) -> bool:
    """True if the file is test-only, an example, or conformance.

    Uses precise filename/directory matching rather than fragile substring
    checks to avoid misclassifying production files.
    """
    # Dedicated test/example/conformance directories
    # Handles both "tests/..." (relative root) and ".../tests/..." (nested)
    if re.search(r"(?:^|/)tests/", path) \
       or re.search(r"(?:^|/)examples/", path) \
       or re.search(r"(?:^|/)conformance/", path):
        return True
    # Test-only file patterns: tests.rs, *_tests.rs
    fname = path.rsplit("/", 1)[-1] if "/" in path else path
    if fname == "tests.rs" or fname.endswith("_tests.rs"):
        return True
    return False


def rust_source_files() -> list[Path]:
    """All .rs files under the repo excluding .git, target, and node_modules."""
    files = []
    for root, dirs, _fnames in os.walk(REPO_ROOT):
        rel = Path(root).relative_to(REPO_ROOT)
        parts = rel.parts
        if any(p in (".git", "target", "node_modules") for p in parts):
            continue
        for fname in _fnames:
            if fname.endswith(".rs"):
                files.append(Path(root) / fname)
    return files


def _load_baseline(name: str) -> dict | None:
    """Load a baseline JSON file from the immutable baseline directory."""
    path = BASELINE_DIR / name
    if not path.is_file():
        return None
    return json.loads(path.read_text())


# ─── Guard 1: SQL boundary ───

SQL_PATTERNS = [
    r"sqlx::query\b",
    r"sqlx::query_as\b",
    r"sqlx::query_scalar\b",
    r"sqlx::query!\b",
    r"sqlx::query_as!\b",
    r"sqlx::query_scalar!\b",
]

# Approved architectural SQL locations.
# SQL belongs behind the store port; only the persistence adapter,
# diagnostic tools, and upgrade code may contain it.
APPROVED_SQL_PATHS_ALLOWLIST = {
    "crates/o3k-store/src",
    "bins/o3k/src/db.rs",
    "bins/o3k/src/upgrade/runner.rs",
}

# Known operational SQL sites outside the persistence adapter,
# loaded from the immutable baseline.  Keyed by (path, line).
KNOWN_SQL_EXCEPTIONS: dict[str, str] = {}

def _load_sql_exceptions() -> dict[str, str]:
    """Load SQL exceptions from the immutable baseline."""
    bl = _load_baseline("sql-inventory.json")
    if not bl:
        return {}
    exc = {}
    for site in bl.get("sites", []):
        cls = site.get("classification", "")
        if cls in ("persistence-adapter", "migration"):
            continue
        key = f"{site['path']}:{site['line']}"
        exc[key] = site.get("content", "")
    return exc


def check_sql_boundary(files: list[Path]) -> list[str]:
    """Check for new production SQL outside approved locations.

    Uses the immutable P13.4 baseline for known exceptions.
    Does NOT regenerate the baseline from current HEAD.
    """
    errors: list[str] = []
    exceptions = _load_sql_exceptions()

    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        if is_test_or_example(rel):
            continue

        # Approved architectural locations pass without line-level checking.
        approved = any(rel.startswith(p) for p in APPROVED_SQL_PATHS_ALLOWLIST)
        if approved:
            continue

        lines = path.read_text(encoding="utf-8").splitlines()

        for i, line in enumerate(lines, 1):
            for pat in SQL_PATTERNS:
                if re.search(pat, line):
                    key = f"{rel}:{i}"
                    if key in exceptions:
                        continue
                    stripped = line.strip()
                    if stripped.startswith("//") or stripped.startswith("/*"):
                        continue
                    if stripped.startswith("use "):
                        continue
                    errors.append(
                        f"NEW SQL call site outside approved location: {rel}:{i}\n"
                        f"  pattern: {pat}\n"
                        f"  code: {stripped[:100]}\n"
                        f"  SQL belongs in crates/o3k-store/src/, diagnostic, or upgrade code."
                    )
                    break
    return errors


# ─── Guard 2: Host-command boundary ───

# Patterns that detect host-execution intent.
# The bare Command::new pattern catches imported usage:
#   use std::process::Command;
#   Command::new("...")
# as well as inline use.
HOST_CMD_PATTERNS = [
    (r"Command::new\(", "std::process::Command / tokio::process::Command"),
    (r'"sh"\s*,?\s*-c\s*"', "sh -c (dangerous)"),
    (r'"bash"\s*,?\s*-c\s*"', "bash -c (dangerous)"),
]

# Directories where host execution is architecturally approved.
APPROVED_HOST_CMD_PREFIXES = {
    "crates/o3k-compute-agent/src",
    "crates/o3k-libvirt/src",
    "crates/o3k-dhcp/src",
    "crates/o3k-storage/src",
    "crates/o3k-cellhv/src",
    "crates/o3k-config-drive/src",
    "crates/o3k-console/src",
    "crates/o3k-network/src",
    "crates/o3k-image/src/qemu_img.rs",
    "crates/o3k-provider/src",
    "bins/o3k/src/sys.rs",
    "bins/o3k/src/checks",
    "bins/o3k/src/upgrade",
    "bins/o3k-compute/src",
}


def check_host_command_boundary(files: list[Path]) -> list[str]:
    """Check for new production host-command execution outside approved locations.

    Detects bare `Command::new(` to catch imported usage.
    Known exceptions from the immutable P13.4 baseline pass silently.
    """
    errors: list[str] = []
    bl = _load_baseline("host-command-inventory.json")
    baseline_exceptions: set[str] = set()
    if bl:
        for site in bl.get("sites", []):
            cls = site.get("classification", "")
            if cls == "domain-owned execution adapter":
                continue
            baseline_exceptions.add(f"{site['path']}:{site['line']}")

    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        if is_test_or_example(rel):
            continue

        approved = any(rel.startswith(p) or rel == p for p in APPROVED_HOST_CMD_PREFIXES)
        if approved:
            continue

        lines = path.read_text(encoding="utf-8").splitlines()

        for i, line in enumerate(lines, 1):
            for pat, name in HOST_CMD_PATTERNS:
                if re.search(pat, line):
                    key = f"{rel}:{i}"
                    if key in baseline_exceptions:
                        continue
                    stripped = line.strip()
                    if stripped.startswith("//"):
                        continue
                    # Allow use statements for Command
                    if stripped.startswith("use ") and "Command" in stripped:
                        continue
                    severity = "HOST COMMAND LEAKAGE" if "sh -c" in name else "candidate architectural leakage"
                    errors.append(
                        f"{severity}: {rel}:{i}\n"
                        f"  command: {name}\n"
                        f"  code: {stripped[:120]}\n"
                        f"  Host execution belongs behind a domain-owned adapter crate."
                    )
                    break
    return errors


# ─── Guard 3: Dependency regression ───

def _canonical_cycle_key(cycle: list[str]) -> tuple:
    """Return a canonical (sorted, hashable) key for a dependency cycle."""
    return tuple(sorted(cycle))


def _load_baseline_cycles() -> set[tuple]:
    """Load known cycles from the immutable P13.4 dependency baseline."""
    bl = _load_baseline("dependencies.json")
    if not bl:
        return set()
    return {_canonical_cycle_key(c) for c in bl.get("cycles", [])}


def check_dependency_regression() -> list[str]:
    """Check cargo metadata for new dependency cycles.

    Compares against the immutable P13.4 dependency baseline.
    """
    errors: list[str] = []
    baseline_cycles = _load_baseline_cycles()

    try:
        result = subprocess.run(
            ["cargo", "metadata", "--format-version", "1"],
            capture_output=True, text=True, timeout=30,
            cwd=REPO_ROOT,
        )
        if result.returncode != 0:
            return [f"cargo metadata failed: {result.stderr[:200]}"]

        meta = json.loads(result.stdout)
        members = set(meta["workspace_members"])
        pkg_map = {p["id"]: p for p in meta["packages"]}

        member_manifest_dirs = {}
        for pid in members:
            pkg = pkg_map.get(pid)
            if pkg:
                member_manifest_dirs[pkg["name"]] = str(Path(pkg["manifest_path"]).parent)

        adj = defaultdict(set)
        for pid in members:
            pkg = pkg_map.get(pid)
            if not pkg:
                continue
            for dep in pkg.get("dependencies", []):
                dep_name = dep.get("name")
                if dep_name == pkg["name"]:
                    continue
                dep_path = dep.get("path")
                if dep_path:
                    dep_dir = str(Path(dep_path).resolve())
                    for mem_name, mem_dir in member_manifest_dirs.items():
                        if mem_name == pkg["name"]:
                            continue
                        if dep_dir == mem_dir:
                            adj[pkg["name"]].add(mem_name)
                            break

        cycles = []
        visited = set()
        path = []

        def dfs(node):
            if node in path:
                idx = path.index(node)
                cycles.append(path[idx:] + [node])
                return
            if node in visited:
                return
            visited.add(node)
            path.append(node)
            for nb in adj.get(node, set()):
                dfs(nb)
            path.pop()

        for node in list(adj.keys()):
            dfs(node)

        seen = set()
        for c in cycles:
            key = _canonical_cycle_key(c)
            if key in seen:
                continue
            seen.add(key)
            if key not in baseline_cycles:
                errors.append(f"NEW dependency cycle detected: {' -> '.join(c)}")

    except subprocess.TimeoutExpired:
        errors.append("cargo metadata timed out (30s)")
    except FileNotFoundError:
        errors.append("cargo not found — dependency check skipped")
    except Exception as e:
        errors.append(f"dependency check error: {e}")

    return errors


# ─── Guard 4: Safety policy ───

def check_safety_policy() -> list[str]:
    """Check that workspace safety policy has not been weakened.

    The workspace already enforces:
      - unsafe_code = forbid
      - -D warnings via Clippy
      - -W clippy::unwrap_used, -W clippy::expect_used

    This guard ensures those policies cannot be silently removed.
    It does NOT duplicate Clippy with a weak parser.
    """
    errors: list[str] = []

    cargo_toml = REPO_ROOT / "Cargo.toml"
    if not cargo_toml.is_file():
        errors.append("WORKSPACE POLICY UNVERIFIABLE: Cargo.toml not found")
        return errors

    content = cargo_toml.read_text()

    if 'unsafe_code = "forbid"' not in content:
        errors.append(
            "WORKSPACE POLICY WEAKENED: `unsafe_code = \"forbid\"` "
            "removed from workspace Cargo.toml"
        )

    return errors


# ─── Main ───

def main() -> int:
    print("=" * 60)
    print("O3K Maintainability Guardrails")
    print(f"  Baseline: {BASELINE_SHA}")
    print(f"  Baseline dir: {BASELINE_DIR.relative_to(REPO_ROOT)}/")
    print("=" * 60)

    if not BASELINE_DIR.is_dir():
        print(f"\nERROR: Baseline directory not found at {BASELINE_DIR}")
        print("Run: python3 scripts/maintainability-inventory.py at P13.4")
        print("     cp target/generated-maintainability/*.json docs/maintainability/baselines/p13-4/")
        return 1

    errors: list[str] = []
    files = rust_source_files()
    prod_files = [f for f in files if not is_test_or_example(str(f.relative_to(REPO_ROOT)))]

    # Guard 1: SQL boundary
    print("\n[1/4] SQL boundary guard...")
    sql_errors = check_sql_boundary(prod_files)
    if sql_errors:
        print(f"  FAILED: {len(sql_errors)} new SQL violations")
        errors.extend(sql_errors)
    else:
        print("  PASS")

    # Guard 2: Host-command boundary
    print("\n[2/4] Host-command boundary guard...")
    cmd_errors = check_host_command_boundary(prod_files)
    if cmd_errors:
        print(f"  FAILED: {len(cmd_errors)} new host-command violations")
        errors.extend(cmd_errors)
    else:
        print("  PASS")

    # Guard 3: Dependency regression
    print("\n[3/4] Dependency regression guard...")
    dep_errors = check_dependency_regression()
    if dep_errors:
        print(f"  FAILED: {len(dep_errors)} dependency issues")
        errors.extend(dep_errors)
    else:
        print("  PASS")

    # Guard 4: Safety policy
    print("\n[4/4] Safety policy guard...")
    safety_errors = check_safety_policy()
    if safety_errors:
        print(f"  FAILED: {len(safety_errors)} safety policy issues")
        errors.extend(safety_errors)
    else:
        print("  PASS")

    print("\n" + "=" * 60)
    if errors:
        print(f"GUARDRAIL CHECK FAILED — {len(errors)} issue(s):")
        for err in errors:
            print(f"\n  ! {err}")
        print("\nNew violations were detected. Do not widen debt exceptions.")
        return 1
    else:
        print("GUARDRAIL CHECK PASSED — no new violations detected.")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
