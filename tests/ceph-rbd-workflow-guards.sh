#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORKFLOW="${ROOT_DIR}/.github/workflows/real-ceph-rbd-guest.yml"
PROFILE="${ROOT_DIR}/scripts/microceph-testlab-profile.sh"
GATE="${ROOT_DIR}/scripts/real-ceph-rbd-guest-gate.sh"
for path in "${WORKFLOW}" "${PROFILE}" "${GATE}"; do test -f "${path}"; done
bash -n "${PROFILE}" "${GATE}"

python3 - "${WORKFLOW}" "${PROFILE}" "${GATE}" <<'PY'
import pathlib, sys
workflow, profile, gate = (pathlib.Path(item).read_text(encoding="utf-8") for item in sys.argv[1:])
for needle in (
    "workflow_dispatch:",
    "runs-on: [self-hosted, linux, x64, kvm, libvirt, o3k-testlab]",
    'test "${GITHUB_REPOSITORY}" = "kubedoio/o3k-rust"',
    'test "${GITHUB_REF}" = "refs/heads/main"',
    "microceph cluster bootstrap",
    "microceph disk add loop,4G,3",
    "ceph-common",
    "/usr/bin/ceph",
    "/usr/bin/rbd",
    "scripts/microceph-testlab-profile.sh provision",
    "scripts/real-ceph-rbd-guest-gate.sh",
    "scripts/microceph-testlab-profile.sh cleanup",
    "if: always()",
    "if-no-files-found: error",
    "actions/upload-artifact@65462800fd760344b1a7b4382951275a0abb4808",
    "--preserve-env=PATH,CARGO_HOME",
):
    assert needle in workflow, needle
assert "continue-on-error" not in workflow
assert "rm -rf" not in workflow
assert "o3k-storage start" not in workflow
assert "microceph.rbd" not in workflow
assert "/var/snap/microceph/current/conf/ceph.keyring" in workflow
for needle in (
    'rbd pool init "${POOL}"',
    'namespace create "${POOL}/${NAMESPACE}"',
    'refusing to adopt pre-existing pool',
    'namespace still contains images',
    'osd pool delete "${POOL}" "${POOL}"',
    'rmdir -- "${STATE_ROOT}"',
):
    assert needle in profile, needle
for needle in (
    'genisoimage -quiet -output "${seed_iso}"',
    "--rng /dev/urandom",
    "provider snapshot-create",
    "provider snapshot-delete",
    "foreign_ceph_mutation",
    "StrictHostKeyChecking=yes",
    'rmdir -- "${STATE_ROOT}"',
):
    assert needle in gate, needle
assert "ssh_pwauth:" not in gate
print("Ceph RBD workflow guard tests passed")
PY
