#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_TESTLAB_ARTIFACT_DIR:-${ROOT_DIR}/target/testlab-artifacts}"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-cli.XXXXXX")"
mkdir -p "${ARTIFACT_DIR}"
rm -f "${ARTIFACT_DIR}/openstack-cli-result.json" "${ARTIFACT_DIR}/openstack-cli-error.log" \
    "${ARTIFACT_DIR}/server-show.json" "${ARTIFACT_DIR}/server-list.json" \
    "${ARTIFACT_DIR}/console.log" "${ARTIFACT_DIR}/console-error.log"
IMAGE_ID=
NETWORK_ID=
SUBNET_ID=
FLAVOR_ID=
SERVER_ID=
CREATED_IMAGE_ID=
CREATED_NETWORK_ID=
CREATED_SUBNET_ID=
CREATED_FLAVOR_ID=
CREATED_SERVER_ID=
SERVER_NAME=o3k-testlab-server

validate_server_json() {
    local kind="$1" path="$2" expected_id="$3"
    python3 - "${kind}" "${path}" "${expected_id}" <<'PY'
import json
import sys

kind, path, expected_id = sys.argv[1:]
with open(path, encoding="utf-8") as stream:
    value = json.load(stream)

if kind == "show":
    if not isinstance(value, dict) or str(value.get("id", "")) != expected_id:
        raise SystemExit("server show did not identify the created server")
elif kind == "list":
    if not isinstance(value, list) or not any(
        isinstance(row, dict) and str(row.get("id", "")) == expected_id
        for row in value
    ):
        raise SystemExit("server list did not contain the created server")
else:
    raise SystemExit(f"unknown server JSON validation kind: {kind}")
PY
}

validate_server_absent_list() {
    local path="$1" expected_id="$2"
    python3 - "${path}" "${expected_id}" <<'PY'
import json
import sys

path, expected_id = sys.argv[1:]
with open(path, encoding="utf-8") as stream:
    value = json.load(stream)
if not isinstance(value, list):
    raise SystemExit("server list response is not an array")
if any(isinstance(row, dict) and str(row.get("id", "")) == expected_id for row in value):
    raise SystemExit("server list still contains the deleted server")
PY
}

server_is_absent() {
    local server_id="$1"
    local show_error="${DATA_DIR}/server-delete-show.error"
    local list_output="${DATA_DIR}/server-delete-list.json"
    local list_error="${DATA_DIR}/server-delete-list.error"

    if openstack server show "${server_id}" -f json \
        >"${DATA_DIR}/server-delete-show.json" 2>"${show_error}"; then
        return 1
    fi
    if grep -Eiq 'not[ -]?found|could not find|no server|404' "${show_error}"; then
        return 0
    fi
    if ! openstack server list --name "${SERVER_NAME}" -f json \
        >"${list_output}" 2>"${list_error}"; then
        return 1
    fi
    validate_server_absent_list "${list_output}" "${server_id}"
}

resource_is_absent() {
    local resource="$1" resource_id="$2"
    local show_output="${DATA_DIR}/${resource}-delete-show.json"
    local show_error="${DATA_DIR}/${resource}-delete-show.error"

    if openstack "${resource}" show "${resource_id}" -f json \
        >"${show_output}" 2>"${show_error}"; then
        return 1
    fi
    grep -Eiq 'not[ -]?found|could not find|no .*found|404' "${show_error}"
}

delete_resource_and_verify_absent() {
    local resource="$1" resource_id="$2"
    # A failed delete has an unknown outcome. Always observe before deciding
    # that cleanup failed; a timeout may have removed the resource already.
    openstack "${resource}" delete "${resource_id}" >/dev/null 2>&1 || true
    resource_is_absent "${resource}" "${resource_id}"
}

cleanup_resources() {
    local cleanup_ok=1
    if [[ -n "${SERVER_ID}" ]]; then
        openstack server delete --wait "${SERVER_ID}" >/dev/null 2>&1 || cleanup_ok=0
        if ! server_is_absent "${SERVER_ID}"; then
            cleanup_ok=0
        else
            SERVER_ID=
        fi
    fi
    if [[ -n "${FLAVOR_ID}" ]]; then
        if delete_resource_and_verify_absent flavor "${FLAVOR_ID}"; then
            FLAVOR_ID=
        else
            cleanup_ok=0
        fi
    fi
    if [[ -n "${SUBNET_ID}" ]]; then
        if delete_resource_and_verify_absent subnet "${SUBNET_ID}"; then
            SUBNET_ID=
        else
            cleanup_ok=0
        fi
    fi
    if [[ -n "${NETWORK_ID}" ]]; then
        if delete_resource_and_verify_absent network "${NETWORK_ID}"; then
            NETWORK_ID=
        else
            cleanup_ok=0
        fi
    fi
    if [[ -n "${IMAGE_ID}" ]]; then
        if delete_resource_and_verify_absent image "${IMAGE_ID}"; then
            IMAGE_ID=
        else
            cleanup_ok=0
        fi
    fi
    return "$((1 - cleanup_ok))"
}

write_result() {
    local status="$1" reason="$2" cleanup_status="${3:-not_run}"
    python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" "${status}" "${reason}" "${cleanup_status}" \
        "${CREATED_IMAGE_ID}" "${CREATED_NETWORK_ID}" "${CREATED_SUBNET_ID}" \
        "${CREATED_FLAVOR_ID}" "${CREATED_SERVER_ID}" <<'PY'
import json, sys, time
path, status, reason, cleanup_status, image_id, network_id, subnet_id, flavor_id, server_id = sys.argv[1:]
result = {
    "artifact_type": "openstack-cli-e2e",
    "status": status,
    "reason": reason,
    "profile": "libvirt",
    "public_api_only": True,
    "redacted": True,
    "cleanup": {"status": cleanup_status},
    "finished_at": int(time.time()),
}
resources = {
    name: value for name, value in {
        "image_id": image_id,
        "network_id": network_id,
        "subnet_id": subnet_id,
        "flavor_id": flavor_id,
        "server_id": server_id,
    }.items() if value
}
if resources:
    result["resources"] = resources
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

on_exit() {
    local exit_code="$?"
    if ((exit_code != 0)); then
        local cleanup_status=passed
        cleanup_resources || cleanup_status=failed
        write_result failed "CLI workflow failed (exit ${exit_code})" "${cleanup_status}"
    fi
    rm -rf "${DATA_DIR}"
    exit "$exit_code"
}
trap on_exit EXIT

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
IMAGE_PATH="${O3K_TESTLAB_IMAGE_PATH:-}"
if [[ -z "${IMAGE_PATH}" || ! -f "${IMAGE_PATH}" ]]; then
    write_result skipped "O3K_TESTLAB_IMAGE_PATH must point to a local guest image"
    echo "OpenStack CLI workflow skipped: configure O3K_TESTLAB_IMAGE_PATH" >&2
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

if ! openstack token issue >/dev/null 2>&1; then
    write_result skipped "OpenStack endpoint is unavailable or authentication failed"
    echo "OpenStack CLI workflow skipped: endpoint/authentication unavailable" >&2
    exit 0
fi

# The remainder deliberately uses only OpenStack CLI calls. Resource IDs are
# captured in the redacted artifact; credentials and response bodies are not
# uploaded. The public CLI does not expose operation IDs here.
IMAGE_ID="$(openstack image create o3k-testlab-image --file "${IMAGE_PATH}" --disk-format raw --container-format bare -f value -c id)"
CREATED_IMAGE_ID="${IMAGE_ID}"
NETWORK_ID="$(openstack network create o3k-testlab-network -f value -c id)"
CREATED_NETWORK_ID="${NETWORK_ID}"
SUBNET_ID="$(openstack subnet create --network "${NETWORK_ID}" --subnet-range 192.0.2.0/29 o3k-testlab-subnet -f value -c id)"
CREATED_SUBNET_ID="${SUBNET_ID}"
FLAVOR_ID="$(openstack flavor create o3k-testlab-flavor --ram 512 --disk 10 --vcpus 1 -f value -c id)"
CREATED_FLAVOR_ID="${FLAVOR_ID}"
SERVER_ID="$(openstack server create --wait --image "${IMAGE_ID}" --flavor "${FLAVOR_ID}" --network "${NETWORK_ID}" "${SERVER_NAME}" -f value -c id)"
CREATED_SERVER_ID="${SERVER_ID}"
openstack server show "${SERVER_ID}" -f json >"${ARTIFACT_DIR}/server-show.json"
validate_server_json show "${ARTIFACT_DIR}/server-show.json" "${SERVER_ID}"
openstack server list --name "${SERVER_NAME}" -f json >"${ARTIFACT_DIR}/server-list.json"
validate_server_json list "${ARTIFACT_DIR}/server-list.json" "${SERVER_ID}"
for _ in $(seq 1 "${O3K_TESTLAB_CONSOLE_ATTEMPTS:-30}"); do
    if openstack console log show "${SERVER_ID}" -f value >"${ARTIFACT_DIR}/console.log" 2>/dev/null \
        && [[ -s "${ARTIFACT_DIR}/console.log" ]]; then
        break
    fi
    sleep "${O3K_TESTLAB_CONSOLE_INTERVAL_SECONDS:-1}"
done
[[ -s "${ARTIFACT_DIR}/console.log" ]]
tail -c 65536 "${ARTIFACT_DIR}/console.log" >"${ARTIFACT_DIR}/console.log.tmp"
mv "${ARTIFACT_DIR}/console.log.tmp" "${ARTIFACT_DIR}/console.log"
openstack server stop --wait "${SERVER_ID}"
openstack server start --wait "${SERVER_ID}"
openstack server reboot --hard --wait "${SERVER_ID}"
openstack server delete --wait "${SERVER_ID}"
if ! server_is_absent "${SERVER_ID}"; then
    echo "server deletion was not verified" >&2
    exit 1
fi
SERVER_ID=
delete_resource_and_verify_absent flavor "${FLAVOR_ID}"
FLAVOR_ID=
delete_resource_and_verify_absent subnet "${SUBNET_ID}"
SUBNET_ID=
delete_resource_and_verify_absent network "${NETWORK_ID}"
NETWORK_ID=
delete_resource_and_verify_absent image "${IMAGE_ID}"
IMAGE_ID=
write_result passed "CLI lifecycle completed" passed
