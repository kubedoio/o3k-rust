#!/usr/bin/env bash
set -Eeuo pipefail

# Guard regression tests for the protected real Cinder service-testbed
# workflow (scripts/real-cinder-pre-run-guard.sh and
# scripts/real-cinder-post-run-guard.sh).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-real-cinder-guards.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

# Name prefixes are not ownership proofs.  Keep the external-service runner
# from regressing to a host-wide domain/link sweep during failure cleanup.
python3 - "${ROOT_DIR}/scripts/real-cinder-testbed-runner.sh" <<'PY'
from pathlib import Path
import sys
text = Path(sys.argv[1]).read_text(encoding="utf-8")
assert "grep '^o3k-'" not in text
assert 'grep -E \'^o3k-\'' not in text
PY

ARTIFACT_DIR="${WORK_DIR}/artifacts"
STATE_BASE="${WORK_DIR}/state"
mkdir -p "${ARTIFACT_DIR}" "${STATE_BASE}"

write_capability() {
    python3 - "${ARTIFACT_DIR}/runner-capabilities.json" <<'PY'
import json, sys
json.dump({"artifact_type": "runner-capabilities", "schema_version": 1,
           "status": "passed", "redacted": True, "finished_at": 1,
           "workflow_run_id": "guard-run-1", "workflow_run_attempt": "1",
           "source_commit": "0123456789abcdef0123456789abcdef01234567"},
          open(sys.argv[1], "w", encoding="utf-8"))
PY
}

export O3K_REAL_HOST_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_REAL_HOST_CAPABILITY_OUTPUT="${ARTIFACT_DIR}/runner-capabilities.json"
export O3K_CINDER_STATE_BASE="${STATE_BASE}"
export O3K_REAL_HOST_WORKFLOW_RUN_ID=guard-run-1
export O3K_REAL_HOST_WORKFLOW_RUN_ATTEMPT=1
export GITHUB_SHA=0123456789abcdef0123456789abcdef01234567
export GITHUB_REPOSITORY=kubedoio/o3k-rust
export GITHUB_EVENT_NAME=workflow_dispatch
export GITHUB_HEAD_REF=
export GITHUB_BASE_REF=
export GITHUB_REF=refs/heads/main

# Ready on a clean baseline with a passing capability probe.
write_capability
bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "ready", value
assert value["capability_status"] == "passed", value
assert value["redacted"] is True, value
PY

# Non-canonical repository blocks.
GITHUB_REPOSITORY=attacker/o3k-rust
if bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "non-canonical repository was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "non_canonical_repository", value
PY
GITHUB_REPOSITORY=kubedoio/o3k-rust

# Non-main source ref blocks.
GITHUB_REF=refs/heads/feature-untrusted
if bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "non-main source ref was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "untrusted_source_ref", value
PY
GITHUB_REF=refs/heads/main

# Fork PR context blocks.
GITHUB_HEAD_REF=feature
if bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "fork PR context was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "untrusted_fork_context", value
PY
GITHUB_HEAD_REF=

# Missing capability probe blocks.
rm -f "${ARTIFACT_DIR}/runner-capabilities.json"
if bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "missing capability probe was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "capability_probe_unavailable", value
PY
write_capability

# Stale run-owned state blocks.
mkdir -p "${STATE_BASE}/stale-run"
if bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "stale run-owned state was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "stale_run_owned_state", value
assert "stale-run" in value["stale_state_dirs"], value
PY
rm -rf "${STATE_BASE}/stale-run"
bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"

# Stale run-owned host resources block (a prior aborted run left a VG, loop
# device, database/user, or RabbitMQ user/vhost behind). Fake host tooling is
# injected so the test is deterministic and does not depend on real host state.
FAKE_HOST_BIN="${WORK_DIR}/fake-host-bin"
mkdir -p "${FAKE_HOST_BIN}"
cat > "${FAKE_HOST_BIN}/vgs" <<'SH'
#!/usr/bin/env bash
if [[ "${O3K_FAKE_STALE_VG:-false}" == true ]]; then echo '  o3k-vg-stale'; fi
SH
cat > "${FAKE_HOST_BIN}/losetup" <<'SH'
#!/usr/bin/env bash
if [[ "${O3K_FAKE_STALE_LOOP:-false}" == true ]]; then echo '/dev/loop9: [0]:4096 (/var/lib/o3k-cinder-testbed/x/o3k-vg-x.img)'; fi
SH
cat > "${FAKE_HOST_BIN}/mysql" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *"SHOW DATABASES"* && "${O3K_FAKE_STALE_DB:-false}" == true ]]; then echo 'o3k_cinder_stale'; fi
if [[ "$*" == *"SELECT User FROM mysql.user"* && "${O3K_FAKE_STALE_DB_USER:-false}" == true ]]; then echo 'o3k_cinder_stale'; fi
SH
cat > "${FAKE_HOST_BIN}/rabbitmqctl" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *"list_vhosts"* && "${O3K_FAKE_STALE_RABBIT:-false}" == true ]]; then echo -e 'Listing vhosts ...\no3k_cinder_stale'; fi
if [[ "$*" == *"list_users"* && "${O3K_FAKE_STALE_RABBIT:-false}" == true ]]; then echo -e 'Listing users ...\no3k_cinder_stale\t[]'; fi
SH
cat > "${FAKE_HOST_BIN}/virsh" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *"list --all --name"* ]]; then
  echo 'instance-00000001'
  if [[ "${O3K_FAKE_STALE_DOMAIN:-false}" == true ]]; then echo 'o3k-d50d1159b9b44cf5c3d7'; fi
fi
SH
cat > "${FAKE_HOST_BIN}/ip" <<'SH'
#!/usr/bin/env bash
if [[ "$*" == *"-o link show"* ]]; then
  echo '2: ens3: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\    link/ether 52:54:00:12:34:56 brd ff:ff:ff:ff:ff:ff'
  if [[ "${O3K_FAKE_STALE_LINK:-false}" == true ]]; then
    echo '9: o3k-br0: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue state UP mode DEFAULT group default qlen 1000\    link/ether b2:d0:21:1f:31:71 brd ff:ff:ff:ff:ff:ff'
    echo '10: o3ktap-f85a1efb: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc noqueue master o3k-br0 state UP mode DEFAULT group default qlen 1000\    link/ether 02:03:f3:47:74:2d brd ff:ff:ff:ff:ff:ff'
  fi
fi
SH
chmod +x "${FAKE_HOST_BIN}"/*

export O3K_FAKE_STALE_VG=true
if PATH="${FAKE_HOST_BIN}:${PATH}" bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "stale run-owned LVM VG was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked", value
assert value["reason"] == "stale_run_owned_host_resources", value
assert any(r["resource"] == "lvm_vg" and r["name"] == "o3k-vg-stale" for r in value["stale_host_resources"]), value
PY
unset O3K_FAKE_STALE_VG

export O3K_FAKE_STALE_LOOP=true O3K_FAKE_STALE_DB=true O3K_FAKE_STALE_DB_USER=true O3K_FAKE_STALE_RABBIT=true
if PATH="${FAKE_HOST_BIN}:${PATH}" bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "stale run-owned host resources were accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "stale_run_owned_host_resources", value
kinds = {r["resource"] for r in value["stale_host_resources"]}
assert kinds == {"loop_device", "mariadb_database", "mariadb_user", "rabbitmq_vhost", "rabbitmq_user"}, kinds
PY
unset O3K_FAKE_STALE_LOOP O3K_FAKE_STALE_DB O3K_FAKE_STALE_DB_USER O3K_FAKE_STALE_RABBIT

# Stale run-owned compute leftovers (a predecessor died between server create
# and cleanup): libvirt domains, bridges, and TAPs must block the run while
# foreign resources (instance-* domains, host links) never match.
export O3K_FAKE_STALE_DOMAIN=true O3K_FAKE_STALE_LINK=true
if PATH="${FAKE_HOST_BIN}:${PATH}" bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"; then
    echo "stale run-owned compute resources were accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "blocked" and value["reason"] == "stale_run_owned_host_resources", value
kinds = {(r["resource"], r["name"]) for r in value["stale_host_resources"]}
assert ("libvirt_domain", "o3k-d50d1159b9b44cf5c3d7") in kinds, kinds
assert ("network_interface", "o3k-br0") in kinds, kinds
assert ("network_interface", "o3ktap-f85a1efb") in kinds, kinds
assert not any("instance-" in name or "ens3" in name for _, name in kinds), kinds
PY
unset O3K_FAKE_STALE_DOMAIN O3K_FAKE_STALE_LINK

# Clean host resources: guard is ready again.
PATH="${FAKE_HOST_BIN}:${PATH}" bash "${ROOT_DIR}/scripts/real-cinder-pre-run-guard.sh"
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "ready", value
PY

# Post-run guard passes on clean evidence.
export O3K_REAL_HOST_WORKFLOW_STEP_STATUS=success
export O3K_STATE_ROOT="${STATE_BASE}/run-1"
mkdir -p "${O3K_STATE_ROOT}/evidence-1"
cat > "${O3K_STATE_ROOT}/evidence-1/foreign-state-before.json" <<'EOF'
{"run_id": "guard-run-1", "foreign": ["hash-a"]}
EOF
cat > "${O3K_STATE_ROOT}/evidence-1/foreign-state-after.json" <<'EOF'
{"run_id": "guard-run-1", "foreign_unchanged": true, "run_owned_resources_remaining": [], "cleanup_status": "passed"}
EOF
cat > "${O3K_STATE_ROOT}/evidence-1/evidence.yaml" <<'EOF'
profile: real-external-cinder-Gazpacho-service-under-test
cinder_version: "Cinder 28.0.0 (2026.1 Gazpacho)"
EOF
bash "${ROOT_DIR}/scripts/real-cinder-post-run-guard.sh"
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "passed", value
assert value["cleanup_status"] == "passed", value
assert value["foreign_unchanged"] is True, value
assert value["run_owned_resources_remaining"] == [], value
PY

# Post-run guard fails when run-owned resources remain.
export O3K_STATE_ROOT="${STATE_BASE}/run-2"
mkdir -p "${O3K_STATE_ROOT}/evidence-2"
cat > "${O3K_STATE_ROOT}/evidence-2/foreign-state-before.json" <<'EOF'
{"run_id": "guard-run-1"}
EOF
cat > "${O3K_STATE_ROOT}/evidence-2/foreign-state-after.json" <<'EOF'
{"run_id": "guard-run-1", "foreign_unchanged": true, "run_owned_resources_remaining": ["lvm_vg:o3k-vg-run"], "cleanup_status": "failed"}
EOF
cat > "${O3K_STATE_ROOT}/evidence-2/evidence.yaml" <<'EOF'
profile: real-external-cinder-Gazpacho-service-under-test
EOF
if bash "${ROOT_DIR}/scripts/real-cinder-post-run-guard.sh"; then
    echo "remaining run-owned resource was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "failed", value
assert value["reason"] == "run_owned_resources_remaining", value
assert value["run_owned_resources_remaining"] == ["lvm_vg:o3k-vg-run"], value
PY

# Post-run guard fails when foreign state changed.
export O3K_STATE_ROOT="${STATE_BASE}/run-3"
mkdir -p "${O3K_STATE_ROOT}/evidence-3"
cat > "${O3K_STATE_ROOT}/evidence-3/foreign-state-before.json" <<'EOF'
{"run_id": "guard-run-1"}
EOF
cat > "${O3K_STATE_ROOT}/evidence-3/foreign-state-after.json" <<'EOF'
{"run_id": "guard-run-1", "foreign_unchanged": false, "run_owned_resources_remaining": [], "cleanup_status": "passed"}
EOF
cat > "${O3K_STATE_ROOT}/evidence-3/evidence.yaml" <<'EOF'
profile: real-external-cinder-Gazpacho-service-under-test
EOF
if bash "${ROOT_DIR}/scripts/real-cinder-post-run-guard.sh"; then
    echo "changed foreign state was accepted" >&2
    exit 1
fi
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "failed", value
assert value["reason"] == "foreign_state_changed", value
PY

# Post-run guard reports skipped when the Cinder step did not run.
export O3K_REAL_HOST_WORKFLOW_STEP_STATUS=skipped
export O3K_STATE_ROOT="${STATE_BASE}/run-4"
bash "${ROOT_DIR}/scripts/real-cinder-post-run-guard.sh"
python3 - "${ARTIFACT_DIR}/real-cinder-workflow-result.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["status"] == "skipped", value
PY

echo "real-cinder workflow guard tests passed"
