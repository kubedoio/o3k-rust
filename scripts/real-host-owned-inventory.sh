#!/usr/bin/env bash
set -Eeuo pipefail

if (($# != 1)); then
    echo "usage: $0 OUTPUT_JSON" >&2
    exit 2
fi
OUTPUT_PATH="$1"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-owned-inventory.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

write_unavailable() {
    local reason="$1"
    python3 - "${OUTPUT_PATH}" "${reason}" <<'PY'
import json, sys
path, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as output:
    json.dump({"status": "unavailable", "reason": reason, "redacted": True}, output, indent=2)
    output.write("\n")
PY
}

domain_output="${WORK_DIR}/domains"
if ! virsh -c qemu:///system list --all --name >"${domain_output}" 2>/dev/null; then
    write_unavailable virsh_inventory_failed
    exit 1
fi

openstack_status=not_checked
if [[ "${O3K_REAL_HOST_OPENSTACK_INVENTORY:-false}" == true && -n "${OS_PASSWORD:-}" ]]; then
    openstack_status=available
    declare -A resource_commands=(
        [server]="server list --name o3k-testlab-server -f value -c ID"
        [image]="image list --name o3k-testlab-image -f value -c ID"
        [network]="network list --name o3k-testlab-network -f value -c ID"
        [subnet]="subnet list --name o3k-testlab-subnet -f value -c ID"
        [flavor]="flavor list --name o3k-testlab-flavor -f value -c ID"
    )
    for resource in "${!resource_commands[@]}"; do
        if ! env -u OS_CLOUD -u OS_CLIENT_CONFIG_FILE \
            openstack ${resource_commands["${resource}"]} >"${WORK_DIR}/${resource}" 2>/dev/null; then
            write_unavailable openstack_inventory_failed
            exit 1
        fi
    done
else
    for resource in server image network subnet flavor; do
        : >"${WORK_DIR}/${resource}"
    done
fi

python3 - "${OUTPUT_PATH}" "${domain_output}" "${openstack_status}" \
    "${WORK_DIR}/server" "${WORK_DIR}/image" "${WORK_DIR}/network" \
    "${WORK_DIR}/subnet" "${WORK_DIR}/flavor" <<'PY'
import json, re, sys
path, domain_path, openstack_status, *resource_paths = sys.argv[1:]
safe_id = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._:-]*$")
domains = []
for line in open(domain_path, encoding="utf-8"):
    value = line.strip()
    if value.startswith("o3k-"):
        if not safe_id.fullmatch(value):
            raise SystemExit("unsafe owned domain name")
        domains.append(value)
resources = {}
for name, resource_path in zip(("server", "image", "network", "subnet", "flavor"), resource_paths):
    values = []
    for line in open(resource_path, encoding="utf-8"):
        value = line.strip()
        if value:
            if not safe_id.fullmatch(value):
                raise SystemExit("unsafe OpenStack resource identity")
            values.append(value)
    resources[name] = sorted(set(values))
with open(path, "w", encoding="utf-8") as output:
    json.dump({"status": "available", "redacted": True, "domains": sorted(set(domains)),
               "openstack": {"status": openstack_status, "resources": resources}},
              output, indent=2)
    output.write("\n")
PY
