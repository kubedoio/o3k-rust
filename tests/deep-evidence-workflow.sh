#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
workflow="${repo_root}/.github/workflows/deep-evidence.yml"
spec="${repo_root}/docs/specs/SPEC-0018-toolchain-and-test-evidence-governance.md"

python3 - "${workflow}" "${spec}" <<'PY'
import pathlib
import re
import sys

workflow = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
spec = pathlib.Path(sys.argv[2]).read_text(encoding="utf-8")

assert "workflow_dispatch:" in workflow
assert "schedule:" in workflow
assert "cron: '17 3 * * 6'" in workflow
assert "permissions:\n  contents: read" in workflow
assert "timeout-minutes: 45" in workflow
assert "timeout-minutes: 30" in workflow
assert "CARGO_LLVM_COV_VERSION: '0.6.16'" in workflow
assert "MIRI_TOOLCHAIN: nightly-2026-07-20" in workflow
assert "cargo install cargo-llvm-cov --version \"${CARGO_LLVM_COV_VERSION}\" --locked" in workflow
assert "rustup toolchain install \"${MIRI_TOOLCHAIN}\" --profile minimal --component miri" in workflow
assert "cargo +\"${MIRI_TOOLCHAIN}\" miri test -p o3k-domain --lib" in workflow
assert "cargo llvm-cov --workspace --all-features --lcov" in workflow
assert "source_commit" in workflow
assert "github_sha" in workflow
assert "lockfile_sha256" in workflow
assert "toolchain_file_sha256" in workflow
assert "actions/upload-artifact@" in workflow
assert "retention-days: 14" in workflow
assert workflow.count("if-no-files-found: error") == 2
assert workflow.count("continue-on-error: true") == 2
assert workflow.count('test \"${') >= 2
assert 'assert record["result"] == "passed"' in workflow
assert 'assert record["evidence_state"] == "partial"' in workflow
assert "no API, compatibility, release, or protected-host claim" in workflow
assert "no API, compatibility, release, or protected-host claim" in workflow
assert "An unsuccessful deep-evidence lane" in spec
assert "cargo-llvm-cov 0.6.16" in spec
assert "nightly-2026-07-20" in spec
assert "deep-evidence.yml" in spec

for line in workflow.splitlines():
    if "uses:" in line:
        assert re.search(r"uses:\s+[^\s]+@[0-9a-f]{40}(?:\s+#.*)?$", line), line

print("deep-evidence workflow contract passed")
PY
