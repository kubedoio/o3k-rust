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
    "CIRROS_IMAGE_URL: https://download.cirros-cloud.net/0.6.3/cirros-0.6.3-x86_64-disk.img",
    "CIRROS_IMAGE_SHA256: 7d6355852aeb6dbcd191bcda7cd74f1536cfe5cbf8a10495a7283a8396e4b75b",
    "ssh-keygen -q -t ed25519",
    "Remove exact disposable guest inputs",
    "scripts/lvm-testlab-profile.sh provision",
    "scripts/real-lvm-guest-gate.sh",
    "scripts/lvm-testlab-profile.sh cleanup",
    "if: always()",
    "if-no-files-found: error",
    "actions/upload-artifact@65462800fd760344b1a7b4382951275a0abb4808",
)
for needle in required:
    assert needle in text, needle
assert "continue-on-error" not in text
assert "rm -rf" not in text
assert "o3k-storage start" not in text
assert "O3K_LVM_GUEST_IMAGE_PATH: ${{ vars." not in text
assert "O3K_LVM_GUEST_SSH_PRIVATE_KEY: ${{ secrets." not in text
PY
grep -Fq -- "--cloud-init" "${SCRIPT}"
grep -Fq -- 'rmdir -- "${STATE_ROOT}"' "${SCRIPT}"
grep -Fq -- 'install -m 0755 -d "${ARTIFACT_DIR}"' "${SCRIPT}"
grep -Fq -- 'O3K_EPHEMERAL_KEY' "${SCRIPT}"
grep -Fq -- 'domain_mac()' "${SCRIPT}"
grep -Fq -- 'tolower($3) == tolower(wanted_mac)' "${SCRIPT}"
grep -Fq -- 'ACTIVE_SSH_HOST="${candidate_host}"' "${SCRIPT}"
grep -Fq -- 'SSH_HOST_OVERRIDE' "${SCRIPT}"
grep -Fq -- 'reset_guest_connection_state' "${SCRIPT}"
grep -Fq -- 'guest readiness diagnostics:' "${SCRIPT}"
grep -Fq -- 'if [[ "${1:-}" == cleanup ]]' "${SCRIPT}"
grep -Fq -- 'Clean up exact real guest resources' "${WORKFLOW}"
if grep -Fq -- 'ssh_pwauth:' "${SCRIPT}"; then
    echo "cloud-config YAML must not be used for CirrOS userdata" >&2
    exit 1
fi

echo "real LVM guest workflow guard tests passed"
