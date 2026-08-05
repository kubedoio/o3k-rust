#!/usr/bin/env bash
set -Eeuo pipefail

# Shared stale run-owned host-state guard for the real Cinder testbed.
#
# Detects leftovers from prior runs and exits 1 when any are found. It never
# deletes, stops, or mutates anything: run-owned resources may still be
# referenced by a hung predecessor, and unknown resources must never be
# removed automatically. Both the protected pre-run guard
# (scripts/real-cinder-pre-run-guard.sh) and the local runner
# (scripts/real-cinder-testbed-runner.sh) call this script so both paths
# enforce identical host-state isolation before any mutation.
#
# Checks (run-owned o3k- prefixes only; foreign state is never matched):
#   - LVM volume groups (o3k-vg-*) and loop devices with o3k backing files
#   - MariaDB databases/users (o3k_cinder_*)
#   - RabbitMQ users/vhosts (o3k_cinder_*)
#   - libvirt domains (o3k-*; foreign instance-* domains never match)
#   - network interfaces (o3k-br*, o3ktap-*)
#   - optionally, stale per-run state directories (--include-state-dirs,
#     used by the protected guard which requires a pristine state base; the
#     local runner keeps prior evidence directories and skips this)
#
# Output: a JSON object {"stale": [{"resource": ..., "name": ...}, ...]} on
# stdout. Exit 0 when clean, 1 when stale state was found, 2 on bad usage.
# A probe that fails (tool missing or not runnable) yields no findings for
# that probe, matching the previous inline guard behavior.

INCLUDE_STATE_DIRS=0
for arg in "$@"; do
  case "${arg}" in
    --include-state-dirs) INCLUDE_STATE_DIRS=1 ;;
    *) echo "real-cinder-stale-state-guard: unknown argument: ${arg}" >&2; exit 2 ;;
  esac
done

STATE_BASE="${O3K_CINDER_STATE_BASE:-${O3K_STATE_ROOT:-/var/lib/o3k-cinder-testbed}}"

python3 - "${STATE_BASE}" "${INCLUDE_STATE_DIRS}" <<'PY'
import json, os, subprocess, sys

state_base = sys.argv[1]
include_state_dirs = sys.argv[2] == "1"

def run(args):
    try:
        return subprocess.run(args, capture_output=True, text=True, check=True).stdout
    except Exception:
        return ""

stale = []

if include_state_dirs:
    try:
        entries = sorted(
            entry
            for entry in os.listdir(state_base)
            if os.path.isdir(os.path.join(state_base, entry))
        )[:20]
        for entry in entries:
            stale.append({"resource": "state_dir", "name": entry})
    except OSError:
        pass

for vg in run(["vgs", "--noheadings", "-o", "vg_name"]).split():
    vg = vg.strip()
    if vg.startswith("o3k-vg-"):
        stale.append({"resource": "lvm_vg", "name": vg})
for line in run(["losetup", "-a"]).splitlines():
    if "o3k" in line:
        stale.append({"resource": "loop_device", "name": line.split(":", 1)[0]})
for db in run(["mysql", "-N", "-e", "SHOW DATABASES;"]).split():
    db = db.strip()
    if db.startswith("o3k_cinder_"):
        stale.append({"resource": "mariadb_database", "name": db})
for user in run(["mysql", "-N", "-e", "SELECT User FROM mysql.user;"]).split():
    user = user.strip()
    if user.startswith("o3k_cinder_"):
        stale.append({"resource": "mariadb_user", "name": user})
for vhost in run(["rabbitmqctl", "list_vhosts"]).splitlines()[1:]:
    vhost = vhost.strip()
    if vhost.startswith("o3k_cinder_"):
        stale.append({"resource": "rabbitmq_vhost", "name": vhost})
for user in run(["rabbitmqctl", "list_users"]).splitlines()[1:]:
    user = user.split()[0] if user.split() else ""
    if user.startswith("o3k_cinder_"):
        stale.append({"resource": "rabbitmq_user", "name": user})

# Run-owned compute leftovers. A prior run that died between server create and
# delete leaves its domain, bridge, and TAPs behind; a successor run has a
# fresh ownership root and must refuse to start rather than collide with
# resources it does not own.
for name in run(["virsh", "list", "--all", "--name"]).splitlines():
    name = name.strip()
    if name.startswith("o3k-"):
        stale.append({"resource": "libvirt_domain", "name": name})
for line in run(["ip", "-o", "link", "show"]).splitlines():
    parts = line.split(":", 2)
    if len(parts) < 2:
        continue
    name = parts[1].strip().split("@", 1)[0]
    if name.startswith("o3k-br") or name.startswith("o3ktap-"):
        stale.append({"resource": "network_interface", "name": name})

print(json.dumps({"stale": stale}, indent=2))
if stale:
    raise SystemExit(1)
PY
