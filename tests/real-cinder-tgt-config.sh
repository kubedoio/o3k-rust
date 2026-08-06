#!/usr/bin/env bash
set -euo pipefail

# Regression test for the real-Cinder testbed iSCSI target configuration
# contract (issue #492, run 31050533925).
#
# Proven root cause: Cinder 28.0.0 TgtAdm.create_iscsi_target writes a
# tgt-admin persistence file into volumes_dir and runs
# `tgt-admin --update <iqn>`, which parses only the tgtd config
# (/etc/tgt/targets.conf by default). Without an `include <volumes_dir>/*`
# directive the target name is not in the parsed config, tgt-admin silently
# exits 0 without creating anything, and Cinder then raises NotFound
# (cinder/volume/targets/tgt.py:215) -> VolumeBackendAPIException -> Nova
# attach 503. The testbed runner fix appends that include with a run-owned
# backup and restores it on cleanup.
#
# This test pins the mechanism with a run-owned config file (tgt-admin -c) so
# CI never mutates /etc/tgt/targets.conf, and pins the runner contract so the
# runner and this test cannot drift apart.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/real-cinder-testbed-runner.sh"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-tgt-config.XXXXXX")"

# tgt-admin refuses to run as non-root ("You must be root to run this
# program."); on CI the test runs as the ubuntu user, exactly like the
# cinder-volume privsep helper would need root privileges.
if [ "$(id -u)" -eq 0 ]; then
  TGTADM="tgtadm"
  TGT_ADMIN="tgt-admin"
else
  TGTADM="sudo -n tgtadm"
  TGT_ADMIN="sudo -n tgt-admin"
fi

VOL_UUID="deadbeef-0000-0000-0000-000000000001"
IQN="iqn.2010-10.org.openstack:volume-${VOL_UUID}"

cleanup() {
  local rc=$?
  # Remove any target the test created.
  $TGTADM --lld iscsi --op show --mode target 2>/dev/null \
    | grep -A1 "Target .*: ${IQN}" >/dev/null 2>&1 \
    && tid="$($TGTADM --lld iscsi --op show --mode target 2>/dev/null \
        | sed -n "s/^Target \([0-9]*\): ${IQN}$/\1/p")" \
    && $TGTADM --lld iscsi --op delete --mode target --tid "${tid}" 2>/dev/null || true
  rm -rf -- "${WORK_DIR}"
  exit $rc
}
trap cleanup EXIT

# 1. Runner contract: the runner must append exactly the include matching the
#    declared volumes_dir (they must not drift apart).
grep -q 'include ${CINDER_VOLUMES_DIR}/\*' "${RUNNER}" \
  || { echo "runner lacks the tgtd cinder include guard" >&2; exit 1; }
grep -q 'CINDER_VOLUMES_DIR="/var/lib/cinder/volumes"' "${RUNNER}" \
  || { echo "runner lacks the explicit volumes_dir contract" >&2; exit 1; }
grep -q '^volumes_dir = ${CINDER_VOLUMES_DIR}$' "${RUNNER}" \
  || { echo "runner cinder.conf lacks volumes_dir" >&2; exit 1; }

# 2. Tooling: tgt-admin/tgtadm must exist and tgtd must be reachable.
command -v tgt-admin >/dev/null || { echo "tgt-admin missing (install tgt)" >&2; exit 1; }
command -v tgtadm >/dev/null || { echo "tgtadm missing (install tgt)" >&2; exit 1; }
if ! $TGTADM --lld iscsi --op show --mode target >/dev/null 2>&1; then
  sudo -n systemctl start tgt 2>/dev/null \
    || sudo -n tgtd 2>/dev/null \
    || { echo "tgtd not running and could not be started" >&2; exit 1; }
  sleep 1
fi

# 3. Run-owned volumes dir with the exact Cinder persistence file shape
#    (cinder 28.0.0 cinder/volume/targets/tgt.py VOLUME_CONF) and a file
#    backing store (no loop devices needed; bs_rdwr accepts regular files).
VOLUMES_DIR="${WORK_DIR}/volumes"
CONF_NO_INCLUDE="${WORK_DIR}/targets-noinclude.conf"
CONF_WITH_INCLUDE="${WORK_DIR}/targets.conf"
mkdir -p "${VOLUMES_DIR}" "${WORK_DIR}/empty"
truncate -s 16M "${WORK_DIR}/backing.img"
cat > "${VOLUMES_DIR}/volume-${VOL_UUID}" <<EOF
<target ${IQN}>
    backing-store ${WORK_DIR}/backing.img
    driver iscsi
    write-cache on
    scsi_sn volume-${VOL_UUID}
    scsi_id volume-${VOL_UUID}
</target>
EOF
printf 'include %s/*\n' "${WORK_DIR}/empty" > "${CONF_NO_INCLUDE}"
printf 'include %s/*\n' "${VOLUMES_DIR}" > "${CONF_WITH_INCLUDE}"

target_present() {
  $TGTADM --lld iscsi --op show --mode target 2>/dev/null | grep -q "^Target [0-9]*: ${IQN}$"
}

# 4. Without the include (the bug): tgt-admin --update must exit 0 yet create
#    no target, exactly as observed in run 31050533925.
if target_present; then
  echo "pre-existing target with the test IQN must not exist" >&2
  exit 1
fi
$TGT_ADMIN -c "${CONF_NO_INCLUDE}" --update "${IQN}"
if target_present; then
  echo "tgt-admin created a target without the volumes include (bug not reproduced)" >&2
  exit 1
fi
echo "    reproduced: tgt-admin exits 0 without creating the target when the include is missing"

# 5. With the include (the fix): the target must exist with LUN 1 and the
#    correct backing-store path.
$TGT_ADMIN -c "${CONF_WITH_INCLUDE}" --update "${IQN}"
target_present || { echo "tgt-admin did not create the target with the include" >&2; exit 1; }
TGTADM_OUT="$($TGTADM --lld iscsi --op show --mode target)"
echo "${TGTADM_OUT}" | grep -q 'LUN: 1' || { echo "LUN 1 missing" >&2; exit 1; }
echo "${TGTADM_OUT}" | grep -q "Backing store path: ${WORK_DIR}/backing.img" \
  || { echo "wrong backing-store path" >&2; exit 1; }
echo "    fix verified: include creates the target with LUN 1 and the correct backing store"

echo "real-cinder tgt configuration regression tests passed"
