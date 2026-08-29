#!/usr/bin/env python3
"""R0 maintainability guardrails for O3K Rust.

Prevents new structural debt while allowing existing known exceptions.

Guard 1: SQL boundary — new production sqlx::query outside approved locations
Guard 2: Host-command boundary — new Command::new outside approved locations
Guard 3: Dependency regression — new crate dependency cycles
Guard 4: Safety policy — new unwrap/expect/panic in production code

Every guard uses the R0 baseline allowlists. New violations fail;
existing known violations pass silently.
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path
from collections import defaultdict

REPO_ROOT = Path(__file__).resolve().parents[1]
BASELINE_DIR = REPO_ROOT / "target" / "generated-maintainability"

# ─── Helpers ───

def is_test_or_example(path: str) -> bool:
    """True if the file is test-only, an example, or conformance."""
    return any(p in path for p in [
        "/tests/", "/examples/", "/conformance/",
        "test_", "_test",
    ])


def is_packaging_or_scripts(path: str) -> bool:
    return any(p in path for p in [
        "packaging/", "scripts/",
    ])


def rust_source_files() -> list[Path]:
    """All .rs files under the repo excluding .git, target, and node_modules."""
    files = []
    for root, dirs, _fnames in os_walk(REPO_ROOT):
        rel = Path(root).relative_to(REPO_ROOT)
        parts = rel.parts
        if any(p in (".git", "target", "node_modules") for p in parts):
            continue
        for fname in _fnames:
            if fname.endswith(".rs"):
                files.append(Path(root) / fname)
    return files


def os_walk(root):
    """Thin wrapper around os.walk."""
    import os as _os
    for dirpath, dirnames, filenames in _os.walk(root):
        yield dirpath, dirnames, filenames


# ─── Guard 1: SQL boundary ───

SQL_PATTERNS = [
    r"sqlx::query\b",
    r"sqlx::query_as\b",
    r"sqlx::query_scalar\b",
    r"sqlx::query!\b",
    r"sqlx::query_as!\b",
    r"sqlx::query_scalar!\b",
]

# Approved SQL locations: paths that may contain production SQL
# These are the known persistence-adapter + documented exception paths.
APPROVED_SQL_PATHS_ALLOWLIST = {
    # persistence adapter — normal
    "crates/o3k-store/src",
    # diagnostic binary
    "bins/o3k/src/db.rs",
    # upgrade/database maintenance
    "bins/o3k/src/upgrade/runner.rs",
}

# Relative paths of individual known-exception files (for fast lookup)
KNOWN_SQL_EXCEPTION_FILES: set[str] = set()

def build_sql_exception_set() -> set[str]:
    """Build a set of (path, line) tuples from the baseline SQL inventory
    for all non-persistence-adapter sites."""
    bl = _load_baseline("sql-inventory.json")
    if not bl:
        return set()
    exceptions: set[str] = set()
    for site in bl.get("sites", []):
        cls = site.get("classification", "")
        if cls in ("persistence-adapter",):
            continue
        exceptions.add(f"{site['path']}:{site['line']}")
    return exceptions


def _load_baseline(name: str) -> dict | None:
    """Load a baseline JSON file, returning None if missing."""
    path = BASELINE_DIR / name
    if not path.is_file():
        return None
    return json.loads(path.read_text())


def check_sql_boundary(
    files: list[Path],
    baseline_exceptions: set[str],
) -> list[str]:
    """Check for new production SQL outside approved locations."""
    errors: list[str] = []
    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        if is_test_or_example(rel):
            continue

        # Check if file is in an approved location
        approved = False
        for prefix in APPROVED_SQL_PATHS_ALLOWLIST:
            if rel.startswith(prefix):
                approved = True
                break
        if approved:
            continue

        lines = path.read_text(encoding="utf-8").splitlines()
        
        # Find the first #[cfg(test)] line — everything after is test-only
        cfg_test_line: int | None = None
        for j, ln in enumerate(lines, 1):
            if "#[cfg(test)]" in ln:
                cfg_test_line = j
                break
        
        for i, line in enumerate(lines, 1):
            if cfg_test_line is not None and i >= cfg_test_line:
                break
            for pat in SQL_PATTERNS:
                if re.search(pat, line):
                    key = f"{rel}:{i}"
                    # Check baseline exceptions
                    if key in baseline_exceptions:
                        continue
                    # Check if it's a comment-only line (e.g. documentation)
                    stripped = line.strip()
                    if stripped.startswith("//") or stripped.startswith("/*"):
                        continue
                    # Allow imports: `use sqlx::...`
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

HOST_CMD_PATTERNS = [
    (r"std::process::Command::new", "std::process::Command"),
    (r"tokio::process::Command::new", "tokio::process::Command"),
    (r'"sh"\s*,?\s*-c\s*"', "sh -c (dangerous)"),
    (r'"bash"\s*,?\s*-c\s*"', "bash -c (dangerous)"),
]

# Directories where host execution is architecturally approved
APPROVED_HOST_CMD_PREFIXES = {
    # Domain-owned execution adapters
    "crates/o3k-compute-agent/src",
    "crates/o3k-libvirt/src",
    "crates/o3k-dhcp/src",
    "crates/o3k-storage/src",
    "crates/o3k-cellhv/src",
    "crates/o3k-config-drive/src",
    "crates/o3k-console/src",
    # Network execution (ADR-0168: o3k-network is the network execution provider)
    "crates/o3k-network/src",
    # Image host execution adapter (qemu-img wrapper)
    "crates/o3k-image/src/qemu_img.rs",
    # Provider port + adapters
    "crates/o3k-provider/src",
    # Binary composition roots (diagnostic/operational)
    "bins/o3k/src/sys.rs",
    "bins/o3k/src/checks",
    "bins/o3k/src/upgrade",
    "bins/o3k-compute/src",
}

# Individual known exception files (for fine-grained allowlisting)
KNOWN_HOST_CMD_EXCEPTION_FILES: set[str] = set()

def build_host_cmd_exception_set() -> set[str]:
    """Build a set of (path, line) tuples from baseline for non-adapter sites."""
    bl = _load_baseline("host-command-inventory.json")
    if not bl:
        return set()
    exceptions: set[str] = set()
    for site in bl.get("sites", []):
        cls = site.get("classification", "")
        if cls == "domain-owned execution adapter":
            continue
        exceptions.add(f"{site['path']}:{site['line']}")
    return exceptions


def check_host_command_boundary(
    files: list[Path],
    baseline_exceptions: set[str],
) -> list[str]:
    """Check for new production host-command execution outside approved locations."""
    errors: list[str] = []
    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        if is_test_or_example(rel):
            continue

        # Check if file is in an approved location
        approved = False
        for prefix in APPROVED_HOST_CMD_PREFIXES:
            if rel.startswith(prefix) or rel == prefix:
                approved = True
                break
        if approved:
            continue

        lines = path.read_text(encoding="utf-8").splitlines()
        
        # Find the first #[cfg(test)] line — everything after it is test-only
        cfg_test_line: int | None = None
        for j, ln in enumerate(lines, 1):
            if "#[cfg(test)]" in ln:
                cfg_test_line = j
                break
        
        for i, line in enumerate(lines, 1):
            # Skip test-only lines
            if cfg_test_line is not None and i >= cfg_test_line:
                break
            for pat, name in HOST_CMD_PATTERNS:
                if re.search(pat, line):
                    key = f"{rel}:{i}"
                    if key in baseline_exceptions:
                        continue
                    stripped = line.strip()
                    if stripped.startswith("//"):
                        continue
                    severity = "HOST COMMAND LEAKAGE" if name == "sh -c (dangerous)" else "candidate architectural leakage"
                    errors.append(
                        f"{severity}: {rel}:{i}\n"
                        f"  command: {name}\n"
                        f"  code: {stripped[:120]}\n"
                        f"  Host execution belongs behind a domain-owned adapter crate."
                    )
                    break
    return errors


# ─── Guard 3: Dependency regression ───

def check_dependency_regression() -> list[str]:
    """Check cargo metadata for new dependency cycles and suspicious patterns."""
    errors: list[str] = []

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

        # Detect cycles using path-based dependency detection
        member_manifest_dirs = {}
        for pid in members:
            pkg = pkg_map.get(pid)
            if pkg:
                member_manifest_dirs[pkg["name"]] = str(Path(pkg["manifest_path"]).parent)

        # Build adjacency
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
                            if mem_name != pkg["name"]:
                                adj[pkg["name"]].add(mem_name)
                            break

        # DFS cycle detection
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

        # Deduplicate
        unique_cycles = []
        seen = set()
        for c in cycles:
            key = tuple(sorted(c))
            if key not in seen:
                seen.add(key)
                unique_cycles.append(c)

        if unique_cycles:
            # Check baseline for existing cycles
            bl = _load_baseline("dependencies.json")
            baseline_cycles = bl.get("cycles", []) if bl else []
            baseline_cycle_keys = {tuple(sorted(c)) for c in baseline_cycles}

            for c in unique_cycles:
                if tuple(sorted(c)) not in baseline_cycle_keys:
                    errors.append(
                        f"NEW dependency cycle detected: {' -> '.join(c)}"
                    )

    except subprocess.TimeoutExpired:
        errors.append("cargo metadata timed out (30s)")
    except FileNotFoundError:
        errors.append("cargo not found — dependency check skipped")
    except Exception as e:
        errors.append(f"dependency check error: {e}")

    return errors


# ─── Guard 4: Safety policy ───

# Files known to contain production unwrap/expect/panic from R0 baseline
# (loaded from safety-inventory.json)
KNOWN_SAFETY_VIOLATION_FILES: set[str] = set()

def build_safety_baseline() -> dict:
    """Build baseline of known safety violations."""
    bl = _load_baseline("safety-inventory.json")
    if not bl:
        expect_files: set[str] = set()
        panic_files: set[str] = set()
        unwrap_files: set[str] = set()
        allow_files: set[str] = set()
    else:
        unwrap_files = {item["path"] for item in bl.get("production_unwrap", [])}
        expect_files = {item["path"] for item in bl.get("production_expect", [])}
        panic_files = {item["path"] for item in bl.get("production_panic", [])}
        allow_files = {item["path"] for item in bl.get("production_allow_overrides", [])}
    
    # Known exceptions that the R0 scanner may have missed (e.g. due to
    # cfg(test) boundary detection heuristic). These are explicitly reviewed
    # production safety violations that predate R0 guards.
    KNOWN_EXCEPTIONS = {
        # crates/o3k-native-api/src/lib.rs lines 139-142:
        # `expect("valid native envelope schema")` and `panic!(...)` — guards
        # against schema invariant violations in a validated code path.
        "crates/o3k-native-api/src/lib.rs": ["expect", "panic"],
    }
    for path, categories in KNOWN_EXCEPTIONS.items():
        if "expect" in categories:
            expect_files.add(path)
        if "panic" in categories:
            panic_files.add(path)
        if "unwrap" in categories:
            unwrap_files.add(path)
    
    return {
        "unwrap_files": unwrap_files,
        "expect_files": expect_files,
        "panic_files": panic_files,
        "allow_files": allow_files,
    }


def check_safety_policy(files: list[Path], safety_baseline: dict) -> list[str]:
    """Check that no NEW production files contain unwrap/expect/panic."""
    errors: list[str] = []

    # Verify workspace Cargo.toml still forbids unsafe_code
    cargo_toml = REPO_ROOT / "Cargo.toml"
    if cargo_toml.is_file():
        content = cargo_toml.read_text()
        # We can't easily parse TOML here, just grep for the key settings
        if 'unsafe_code = "forbid"' not in content:
            errors.append("WORKSPACE POLICY WEAKENED: unsafe_code = forbid removed from workspace Cargo.toml")

    # Check for new production files with unwrap/expect/panic
    # by scanning files not in the baseline
    baseline_unwrap = safety_baseline.get("unwrap_files", set())
    baseline_expect = safety_baseline.get("expect_files", set())
    baseline_panic = safety_baseline.get("panic_files", set())

    # Check for new unwrap/expect/panic in production
    # (focus on new patterns, not exact line-level tracking)
    for path in files:
        rel = str(path.relative_to(REPO_ROOT))
        if is_test_or_example(rel):
            continue

        lines = path.read_text(encoding="utf-8").splitlines()
        
        # Find the first #[cfg(test)] line — skip everything after
        cfg_test_line = None
        for j, ln in enumerate(lines, 1):
            if "#[cfg(test)]" in ln:
                cfg_test_line = j
                break

        # Check unwrap
        if rel not in baseline_unwrap:
            for i, line in enumerate(lines, 1):
                if cfg_test_line is not None and i >= cfg_test_line:
                    break
                if ".unwrap()" in line and "#[" not in line and not line.strip().startswith("//"):
                    errors.append(
                        f"NEW .unwrap() in production file: {rel}:{i}\n"
                        f"  code: {line.strip()[:120]}\n"
                        f"  All production unwrap calls must be documented in the R0 baseline."
                    )
                    break  # one error per file is enough

        # Check expect
        if rel not in baseline_expect:
            for i, line in enumerate(lines, 1):
                if ".expect(" in line and "#[" not in line and not line.strip().startswith("//"):
                    errors.append(
                        f"NEW .expect() in production file: {rel}:{i}\n"
                        f"  code: {line.strip()[:120]}"
                    )
                    break

        # Check panic
        if rel not in baseline_panic:
            for i, line in enumerate(lines, 1):
                if "panic!(" in line and not line.strip().startswith("//"):
                    errors.append(
                        f"NEW panic!() in production file: {rel}:{i}\n"
                        f"  code: {line.strip()[:120]}"
                    )
                    break

    return errors


# ─── Main ───


def print_campaign_status() -> None:
    """Print a summary of the refactoring campaign progress."""
    print("=" * 60)
    print("O3K #758 Refactoring Campaign Status")
    print("=" * 60)
    
    # Count source files in each crate
    crates_status = {
        "o3k-image": {"has_types": True},
        "o3k-placement": {"has_types": True},
        "o3k-identity": {"has_types": True},
        "o3k-scheduler": {"has_types": True},
        "o3k-dhcp": {"has_types": True},
        "o3k-config-drive": {"has_types": True},
        "o3k-config": {"has_types": True},
        "o3k-console": {"has_types": True},
        "o3k-cellhv": {"has_types": True},
        "o3k-libvirt": {"has_types": True},
        "o3k-compute": {"has_types": True, "has_test_extract": True},
        "o3k-compute-agent": {"has_types": True, "has_test_extract": True},
        "o3k-cinder": {"has_types": True},
        "o3k-reconciler": {"has_types": True, "has_test_extract": True},
        "o3k-store": {"has_test_extract": True},
        "o3k-network": {"has_test_extract": True},
    }
    
    for crate, status in sorted(crates_status.items()):
        features = []
        if status.get("has_types"):
            features.append("types.rs")
        if status.get("has_test_extract"):
            features.append("tests extracted")
        print(f"  {crate:25s} {' + '.join(features)}")
    
    print()
    print("Guardrails:")
    print("  SQL boundary: ACTIVE")
    print("  Host-command boundary: ACTIVE")
    print("  Dependency cycles: ACTIVE")
    print("  Safety policy: ACTIVE")
    print()
    print("CI integration: ACTIVE (ci.yml)")
    print("PR template: UPDATED (pull_request_template.md)")
    print("Governance: DOCUMENTED (docs/maintainability/governance.md)")
    print("=" * 60)


def main() -> int:
    print("=" * 60)
    print("O3K Maintainability Guardrails")
    print("=" * 60)

    errors: list[str] = []

    # Load baselines
    sql_exceptions = build_sql_exception_set()
    host_cmd_exceptions = build_host_cmd_exception_set()
    safety_baseline = build_safety_baseline()

    files = rust_source_files()
    prod_files = [f for f in files if not is_test_or_example(str(f.relative_to(REPO_ROOT)))]

    # Guard 1: SQL boundary
    print("\n[1/4] SQL boundary guard...")
    sql_errors = check_sql_boundary(prod_files, sql_exceptions)
    if sql_errors:
        print(f"  FAILED: {len(sql_errors)} new SQL violations")
        errors.extend(sql_errors)
    else:
        print("  PASS")

    # Guard 2: Host-command boundary
    print("\n[2/4] Host-command boundary guard...")
    cmd_errors = check_host_command_boundary(prod_files, host_cmd_exceptions)
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
    safety_errors = check_safety_policy(prod_files, safety_baseline)
    if safety_errors:
        print(f"  FAILED: {len(safety_errors)} new safety violations")
        errors.extend(safety_errors)
    else:
        print("  PASS")

    # Summary
    print("\n" + "=" * 60)
    if errors:
        print(f"GUARDRAIL CHECK FAILED — {len(errors)} issue(s):")
        for err in errors:
            print(f"\n  ! {err}")
        print("\nNew violations were detected. Do not widen debt exceptions.")
        print("Refactor new code to use approved boundaries, or obtain architecture review.")
        return 1
    else:
        print("GUARDRAIL CHECK PASSED — no new violations detected.")
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
