#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"

python3 - "${WORKFLOW}" <<'PY'
import pathlib
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
assert "git fetch origin main:refs/remotes/origin/main" in text
assert "buf breaking --against '.git#branch=origin/main,subdir=proto'" in text
assert "packaging/*.sh tests/*.sh scripts/*.sh" in text
assert "python3 -m compileall -q scripts" in text
assert "github.com/rhysd/actionlint/cmd/actionlint@v1.7.7" in text
assert "run: bash scripts/validate-workflows.sh actionlint" in text
assert "run: bash tests/workflow-validation.sh actionlint" in text
assert "run: bash tests/real-libvirt-harness.sh" in text
assert "run: cargo test --workspace --all-features" in text
assert "run: cargo test --workspace\n" not in text
assert "protobuf-compiler libvirt-dev pkg-config" in text
assert "cargo clean -p virt-sys" in text
assert "git fetch origin main:refs/heads/main" not in text
assert "buf breaking --against '.git#branch=main,subdir=proto'" not in text
PY

echo "CI workflow contract tests passed"
