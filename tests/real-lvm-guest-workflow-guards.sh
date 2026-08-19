#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/real-lvm-guest.yml"
SCRIPT="${ROOT_DIR}/scripts/real-lvm-guest-gate.sh"

test -f "${WORKFLOW}"
test -f "${SCRIPT}"
bash -n "${SCRIPT}"

python3 - "${WORKFLOW}" <<'PY'
import pathlib, sys
text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
required = (
    "workflow_dispatch:",
    "runs-on: [self-hosted, linux, x64, kvm, libvirt, o3k-testlab]",
    'test "${GITHUB_REPOSITORY}" = "kubedoio/o3k-rust"',
    'test "${GITHUB_REF}" = "refs/heads/main"',
    "sudo -n true",
    "O3K_LVM_GUEST_IMAGE_PATH",
    "O3K_LVM_GUEST_SSH_PRIVATE_KEY",
    "scripts/lvm-testlab-profile.sh provision",
    "scripts/real-lvm-guest-gate.sh",
    "scripts/lvm-testlab-profile.sh cleanup",
    "if: always()",
    "if-no-files-found: error",
    "actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607af02",
)
for needle in required:
    assert needle in text, needle
assert "continue-on-error" not in text
assert "rm -rf" not in text
assert "o3k-storage start" not in text
PY

echo "real LVM guest workflow guard tests passed"
