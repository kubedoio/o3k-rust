#!/usr/bin/env bash
set -Eeuo pipefail

# Create and remove only a run-scoped RBD pool and image namespace on a
# pre-provisioned MicroCeph test cluster. The cluster and its OSDs are runner
# infrastructure; this helper never tears down unrelated cluster state.

ACTION="${1:-}"
case "${ACTION}" in
    provision|cleanup|verify) ;;
    *) echo "usage: $0 provision|cleanup|verify" >&2; exit 2 ;;
esac

STATE_BASE="${O3K_CEPH_STATE_BASE:-/var/lib/o3k-ceph-testlab}"
RUN_ID="${O3K_CEPH_RUN_ID:-${GITHUB_RUN_ID:-local-$(date +%s)}}"
RUN_SLUG="$(printf '%s' "${RUN_ID}" | tr -cd '[:alnum:]' | head -c 16)"
STATE_ROOT="${O3K_CEPH_STATE_ROOT:-${STATE_BASE}/${RUN_SLUG}}"
STATE_FILE="${STATE_ROOT}/profile.json"
POOL="${O3K_CEPH_POOL:-o3k-p10-${RUN_SLUG}}"
NAMESPACE="${O3K_CEPH_NAMESPACE:-o3k-${RUN_SLUG}}"
CEPH_BIN="${O3K_CEPH_CEPH_BIN:-microceph.ceph}"
RBD_BIN="${O3K_CEPH_RBD_BIN:-microceph.rbd}"

die() { echo "microceph-testlab-profile: $*" >&2; exit 1; }
valid_name() { [[ "$1" =~ ^[A-Za-z0-9_.-]{1,128}$ ]]; }

validate_inputs() {
    valid_name "${RUN_SLUG}" || die "invalid run slug"
    valid_name "${POOL}" || die "invalid pool name"
    valid_name "${NAMESPACE}" || die "invalid namespace"
    [[ "${STATE_ROOT}" = /* && "${STATE_ROOT}" != "/" && "${STATE_ROOT}" != "/var" && "${STATE_ROOT}" != "/var/lib" ]] \
        || die "unsafe state root"
    [[ "${STATE_ROOT}" != *"/../"* && "${STATE_ROOT}" != */.. && "${STATE_ROOT##*/}" == "${RUN_SLUG}" ]] \
        || die "unsafe state root"
}

require_tools() {
    command -v "${CEPH_BIN}" >/dev/null 2>&1 || die "Ceph command is unavailable: ${CEPH_BIN}"
    command -v "${RBD_BIN}" >/dev/null 2>&1 || die "RBD command is unavailable: ${RBD_BIN}"
}

pool_exists() { "${CEPH_BIN}" osd pool get "${POOL}" size >/dev/null 2>&1; }
namespace_exists() {
    "${RBD_BIN}" namespace ls "${POOL}" --format json 2>/dev/null |
        python3 -c 'import json, sys; rows=json.load(sys.stdin); wanted=sys.argv[1]; raise SystemExit(0 if any(row.get("name") == wanted for row in rows) else 1)' "${NAMESPACE}"
}

write_state() {
    mkdir -p -- "${STATE_ROOT}"
    python3 - "${STATE_FILE}" "${RUN_ID}" "${POOL}" "${NAMESPACE}" <<'PY'
import json, os, sys, tempfile
path, run_id, pool, namespace = sys.argv[1:]
value = {
    "artifact_type": "microceph-rbd-profile",
    "schema_version": 1,
    "status": "provisioned",
    "redacted": True,
    "run_id": run_id,
    "pool": pool,
    "namespace": namespace,
}
fd, temporary = tempfile.mkstemp(prefix=".profile.", dir=os.path.dirname(path), text=True)
try:
    with os.fdopen(fd, "w", encoding="utf-8") as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write("\n")
        output.flush()
        os.fsync(output.fileno())
    os.replace(temporary, path)
except BaseException:
    try: os.unlink(temporary)
    except FileNotFoundError: pass
    raise
PY
}

read_state() {
    [[ -f "${STATE_FILE}" ]] || die "profile state is unavailable"
    python3 - "${STATE_FILE}" "${RUN_ID}" "${POOL}" "${NAMESPACE}" <<'PY'
import json, re, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
if value.get("artifact_type") != "microceph-rbd-profile" or value.get("status") != "provisioned":
    raise SystemExit("invalid profile state")
for actual, expected in zip((value.get("run_id"), value.get("pool"), value.get("namespace")), sys.argv[2:]):
    if actual != expected: raise SystemExit("profile identity mismatch")
for key in ("pool", "namespace"):
    if not re.fullmatch(r"[A-Za-z0-9_.-]{1,128}", value[key]): raise SystemExit("unsafe profile name")
PY
}

provision() {
    require_tools; validate_inputs
    [[ ! -e "${STATE_ROOT}" ]] || die "run-owned state already exists: ${STATE_ROOT}"
    pool_exists && die "refusing to adopt pre-existing pool ${POOL}"
    local pool_created=false
    trap 'if [[ "${pool_created}" == true ]]; then "${CEPH_BIN}" osd pool delete "${POOL}" "${POOL}" --yes-i-really-really-mean-it >/dev/null 2>&1 || true; fi' ERR
    "${CEPH_BIN}" osd pool create "${POOL}" 8 8 >/dev/null
    pool_created=true
    "${CEPH_BIN}" osd pool application enable "${POOL}" rbd >/dev/null
    "${RBD_BIN}" pool init "${POOL}" >/dev/null
    "${RBD_BIN}" namespace create "${POOL}/${NAMESPACE}" >/dev/null
    write_state
    trap - ERR
    echo "provisioned MicroCeph RBD profile: ${STATE_FILE}"
}

verify() {
    require_tools; validate_inputs; read_state
    pool_exists || die "run-scoped pool is missing"
    namespace_exists || die "run-scoped namespace is missing"
    echo "verified MicroCeph RBD profile: ${STATE_FILE}"
}

cleanup() {
    require_tools; validate_inputs; read_state
    pool_exists || die "refusing cleanup of missing pool"
    namespace_exists || die "refusing cleanup of missing namespace"
    images="$(${RBD_BIN} ls --pool "${POOL}" --namespace "${NAMESPACE}" --format json)"
    [[ "${images}" == "[]" ]] || die "run-scoped namespace still contains images"
    "${RBD_BIN}" namespace remove "${POOL}/${NAMESPACE}" >/dev/null
    "${CEPH_BIN}" osd pool delete "${POOL}" "${POOL}" --yes-i-really-really-mean-it >/dev/null
    rm -f -- "${STATE_FILE}"
    rmdir -- "${STATE_ROOT}"
    echo "cleaned MicroCeph RBD profile for ${RUN_ID}"
}

case "${ACTION}" in
    provision) provision ;;
    verify) verify ;;
    cleanup) cleanup ;;
esac
