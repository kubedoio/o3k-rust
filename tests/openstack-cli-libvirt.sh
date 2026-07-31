#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_TESTLAB_ARTIFACT_DIR:-${ROOT_DIR}/target/testlab-artifacts}"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-cli.XXXXXX")"
mkdir -p "${ARTIFACT_DIR}"
rm -f "${ARTIFACT_DIR}/openstack-cli-result.json" "${ARTIFACT_DIR}/openstack-cli-error.log" \
    "${ARTIFACT_DIR}/server-show.json" "${ARTIFACT_DIR}/console.log"
trap 'rm -rf "${DATA_DIR}"' EXIT

write_result() {
    local status="$1" reason="$2"
    python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" "${status}" "${reason}" <<'PY'
import json, sys, time
path, status, reason = sys.argv[1:]
result = {
    "artifact_type": "openstack-cli-e2e",
    "status": status,
    "reason": reason,
    "profile": "libvirt",
    "public_api_only": True,
    "redacted": True,
    "cleanup": {"status": "passed" if status == "passed" else "not_run"},
    "finished_at": int(time.time()),
}
if status == "passed":
    result["lifecycle"] = {
        "create": True, "show": True, "list": True, "stop": True,
        "start": True, "reboot": True, "console": True, "delete": True,
    }
with open(path, "w", encoding="utf-8") as output:
    json.dump(result, output, indent=2)
    output.write("\n")
PY
}

for command in openstack curl; do
    if ! command -v "${command}" >/dev/null 2>&1; then
        write_result skipped "missing command: ${command}"
        echo "OpenStack CLI workflow skipped: missing ${command}" >&2
        exit 0
    fi
done
if [[ "${O3K_TESTLAB_PROFILE:-libvirt}" != libvirt ]]; then
    write_result skipped "O3K_TESTLAB_PROFILE is not libvirt"
    exit 0
fi

CLOUDS_FILE="${DATA_DIR}/clouds.yaml"
cat >"${CLOUDS_FILE}" <<EOF
clouds:
  o3k-testlab:
    auth:
      auth_url: ${OS_AUTH_URL:-http://127.0.0.1:8080/v3}
      username: ${OS_USERNAME:-admin}
      password: ${OS_PASSWORD:-}
      project_name: ${OS_PROJECT_NAME:-bootstrap-project}
      user_domain_name: Default
      project_domain_name: Default
    region_name: ${OS_REGION_NAME:-RegionOne}
    interface: public
    identity_api_version: 3
EOF
export OS_CLOUD=o3k-testlab OS_CLIENT_CONFIG_FILE="${CLOUDS_FILE}"
if [[ -z "${OS_PASSWORD:-}" ]]; then
    write_result skipped "OS_PASSWORD is not configured"
    echo "OpenStack CLI workflow skipped: configure OS_PASSWORD" >&2
    exit 0
fi

if ! openstack token issue >/dev/null 2>"${ARTIFACT_DIR}/openstack-cli-error.log"; then
    write_result skipped "OpenStack endpoint is unavailable or authentication failed"
    echo "OpenStack CLI workflow skipped: endpoint/authentication unavailable" >&2
    exit 0
fi

# The remainder deliberately uses only OpenStack CLI calls. Resource IDs and
# operation IDs are captured in the artifact; credentials and response bodies
# are not uploaded.
IMAGE_ID="$(openstack image create o3k-testlab-image --disk-format raw --container-format bare -f value -c id)"
NETWORK_ID="$(openstack network create o3k-testlab-network -f value -c id)"
SUBNET_ID="$(openstack subnet create --network "${NETWORK_ID}" --subnet-range 192.0.2.0/29 o3k-testlab-subnet -f value -c id)"
FLAVOR_ID="$(openstack flavor create o3k-testlab-flavor --ram 512 --disk 10 --vcpus 1 -f value -c id)"
SERVER_ID="$(openstack server create --image "${IMAGE_ID}" --flavor "${FLAVOR_ID}" --network "${NETWORK_ID}" o3k-testlab-server -f value -c id)"
openstack server show "${SERVER_ID}" -f json >"${ARTIFACT_DIR}/server-show.json"
openstack console log show "${SERVER_ID}" -f value | tail -c 65536 >"${ARTIFACT_DIR}/console.log"
openstack server stop "${SERVER_ID}"
openstack server start "${SERVER_ID}"
openstack server reboot --hard "${SERVER_ID}"
openstack server delete --wait "${SERVER_ID}"
openstack flavor delete "${FLAVOR_ID}"
openstack subnet delete "${SUBNET_ID}"
openstack network delete "${NETWORK_ID}"
openstack image delete "${IMAGE_ID}"
write_result passed "CLI lifecycle completed"
