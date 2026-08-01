#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"

python3 - "${WORKFLOW}" <<'PY'
import pathlib
import re
import sys

text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
assert "git fetch origin main:refs/remotes/origin/main" in text
assert "buf breaking --against '.git#branch=origin/main,subdir=proto'" in text
assert "packaging/*.sh tests/*.sh scripts/*.sh" in text
assert "python3 -m compileall -q scripts" in text
assert "actionlint_1.7.7_linux_amd64.tar.gz" in text
assert "023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757" in text
assert "sha256sum --check --status" in text
assert "run: bash scripts/validate-workflows.sh actionlint" in text
assert "run: bash tests/workflow-validation.sh actionlint" in text
assert "run: bash tests/real-libvirt-harness.sh" in text
assert "run: cargo test --workspace --all-features" in text
assert "run: cargo test --workspace\n" not in text
assert "protobuf-compiler libvirt-dev pkg-config" in text
assert "cargo clean -p virt-sys" in text
assert "git fetch origin main:refs/heads/main" not in text
assert "buf breaking --against '.git#branch=main,subdir=proto'" not in text

for workflow in pathlib.Path(sys.argv[1]).parent.glob("*.y*ml"):
    for line in workflow.read_text(encoding="utf-8").splitlines():
        if "uses:" in line:
            assert re.search(r"uses:\s+[^\s]+@[0-9a-f]{40}(?:\s+#.*)?$", line), line
real_host = pathlib.Path(sys.argv[1]).parent / "real-host-validation.yml"
real_host_text = real_host.read_text(encoding="utf-8")
assert "if: github.repository == 'kubedoio/o3k-rust' && github.ref == 'refs/heads/main'" in real_host_text
assert "target/real-host-workflow-artifacts/console.log" not in real_host_text
assert "target/real-host-workflow-artifacts/server-show.json" not in real_host_text
assert "target/real-host-workflow-artifacts/openstack-cli-result.json" in real_host_text
assert "Download and verify CirrOS image" in real_host_text
assert "CIRROS_IMAGE_URL: https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img" in real_host_text
assert "CIRROS_IMAGE_SHA256: 7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b" in real_host_text
assert "--connect-timeout 15 --max-time 300" in real_host_text
assert "timeout-minutes: 60" in real_host_text
PY

echo "CI workflow contract tests passed"
