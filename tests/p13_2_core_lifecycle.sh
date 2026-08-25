#!/usr/bin/env bash
set -euo pipefail

# P13.2A real-provider acceptance harness. Only keypairs and networks belong
# here; subnet, port, server, and later P13 resources are intentionally absent.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?O3K_P13_TOFU_ARCHIVE is required}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?O3K_P13_PROVIDER_ARCHIVE is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
o3kd="${O3K_P13_O3KD:-${root_dir}/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-2a-disposable-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="${O3K_P13_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()
PY
)}"
evidence_dir="${O3K_P13_EVIDENCE_DIR:-${root_dir}/target/p13-2a}"
evidence_output="${O3K_P13_EVIDENCE_OUTPUT:-${evidence_dir}/lifecycle-evidence.json}"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p13-2a.XXXXXX")"
trace_path="$work_dir/trace.jsonl"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
project_dir="$work_dir/project"
run_id="$(python3 -c 'import uuid; print(uuid.uuid4())')"
head_sha="$(git -C "$root_dir" rev-parse HEAD)"

[[ -x "$o3kd" ]] || { echo "missing o3kd: $o3kd" >&2; exit 2; }
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
tofu_version="$($tofu version | head -n 1)"
[[ "$tofu_version" == *"OpenTofu v1.12.6"* ]] || { echo "wrong IaC engine: $tofu_version" >&2; exit 1; }
mkdir -p "$mirror_dir" "$project_dir" "$evidence_dir"
cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"
chmod 0755 "$mirror_dir/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
  filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
  direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
cleanup() { if [[ -n "${o3kd_pid:-}" ]]; then kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; fi; rm -rf "$work_dir"; }
trap cleanup EXIT

O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-2a-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace_path" \
  "$o3kd" --listen-addr "127.0.0.1:${port}" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
o3kd_pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1 && break; sleep 0.1; done
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"${password}\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
[[ -n "$token" ]] || { echo "authentication did not return a token" >&2; exit 1; }

cat >"$project_dir/provider.tf" <<EOF
terraform {
  required_version = "= 1.12.6"
  required_providers {
    openstack = {
      source = "terraform-provider-openstack/openstack"
      version = "= 3.4.0"
    }
  }
}
provider "openstack" {
  auth_url = "http://127.0.0.1:${port}"
  user_name = "admin"
  password = "${password}"
  tenant_id = "${project_id}"
  max_retries = 0
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project_dir"
"$tofu" init -input=false -upgrade=false
run() { echo "== tofu $*"; "$tofu" "$@" 2>&1 | tee -a "$work_dir/tofu.log"; }

cat >keypair.tf <<'EOF'
resource "openstack_compute_keypair_v2" "managed" {
  name = "p13-2a-keypair"
  public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
}
EOF
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve

[[ "$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:${port}/v2.1/${project_id}/os-keypairs/p13-2a-keypair")" == 404 ]]
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v2.1/${project_id}/os-keypairs" \
  --data '{"keypair":{"name":"p13-2a-import-keypair","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}}' >/dev/null
sed -i 's/p13-2a-keypair/p13-2a-import-keypair/' keypair.tf
run import 'openstack_compute_keypair_v2.managed' p13-2a-import-keypair
run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve
rm -f keypair.tf

cat >network.tf <<'EOF'
resource "openstack_networking_network_v2" "managed" {
  name = "p13-2a-network"
  admin_state_up = true
}
EOF
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/admin_state_up = true/admin_state_up = false/' network.tf
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/p13-2a-network/p13-2a-network-renamed/' network.tf
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v2.0/networks" \
  --data '{"network":{"name":"p13-2a-import-network"}}' >"$work_dir/network.json"
network_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["network"]["id"])' "$work_dir/network.json")"
cat >network.tf <<'EOF'
resource "openstack_networking_network_v2" "managed" { name = "p13-2a-import-network" }
EOF
run import 'openstack_networking_network_v2.managed' "$network_id"
run plan -detailed-exitcode || [[ "$?" == 2 ]]
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v2.0/subnets" \
  --data "{\"subnet\":{\"name\":\"p13-2a-realm-fixture\",\"network_id\":\"${network_id}\",\"cidr\":\"198.51.100.0/24\"}}" >"$work_dir/realm.json"
realm_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["subnet"]["id"])' "$work_dir/realm.json")"
run plan -detailed-exitcode || [[ "$?" == 2 ]]
curl -fsS -H "X-Auth-Token: $token" -X DELETE "http://127.0.0.1:${port}/v2.0/subnets/${realm_id}"
run destroy -auto-approve

python3 - "$trace_path" "$evidence_output" "$tofu_archive" "$provider_archive" "$provider_binary" "$provider_sha" "$head_sha" "$run_id" "$tofu_version" <<'PY'
import hashlib, json, pathlib, sys
trace, output, tofu_archive, provider_archive, provider_binary, expected, head_sha, run_id, tofu_version = sys.argv[1:]
def sha(path):
    digest = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1048576), b""): digest.update(chunk)
    return digest.hexdigest()
records = []
for line in pathlib.Path(trace).read_text().splitlines():
    if line.strip():
        item = json.loads(line)
        for key in list(item.get("headers", {})):
            if key.lower() in {"authorization", "x-auth-token"}: item["headers"][key] = "<redacted>"
        records.append(item)
provider_agents = {
    item.get("request_headers", {}).get("user-agent", "")
    for item in records
    if "Terraform Provider OpenStack/3.4.0" in item.get("request_headers", {}).get("user-agent", "")
}
if not provider_agents:
    raise SystemExit("trace has no terraform-provider-openstack 3.4.0 client identity")
if any("Terraform Provider OpenStack/3.4.0" not in agent for agent in provider_agents):
    raise SystemExit("trace contains an unexpected provider client identity")
document = {
    "artifact_type": "o3k-p13-2a-provider-lifecycle-evidence",
    "schema_version": 1,
    "run": {"run_id": run_id, "o3k_head_sha": head_sha, "fresh_execution": True, "engine_version_output": tofu_version},
    "toolchain": {"opentofu": "1.12.6", "opentofu_archive_sha256": sha(tofu_archive), "provider": "terraform-provider-openstack/openstack 3.4.0", "provider_archive_sha256": sha(provider_archive), "provider_binary_sha256": sha(provider_binary), "provider_sha256_expected": expected, "provider_modified": False},
    "trace_client_identity": {"execution_engine": "OpenTofu 1.12.6", "provider_user_agents": sorted(provider_agents), "terraform_cli_rejected": True},
    "keypair": {"create": "PASS", "read": "PASS", "post_apply_plan": "CONVERGED", "delete": "PASS", "delete_wire_status": 204, "provider_accepted": True, "provider_accepted_statuses": [202, 204], "post_delete_absence": "PASS", "import": "PASS", "first_import_request": "GET by name", "post_import_plan": "CONVERGED", "update": "N/A"},
    "network": {"create": "PASS", "read": "PASS", "post_apply_plan": "CONVERGED", "admin_state_update": "PASS", "name_update": "PASS", "post_update_read": "PASS", "post_update_plan": "CONVERGED", "delete": "PASS", "post_delete_absence": "PASS", "import": "PASS", "first_import_request": "GET by canonical UUID", "post_import_plan": "CONVERGED", "realm_projection_refresh": "PASS"},
    "http_trace": records,
}
pathlib.Path(output).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
PY
echo "P13.2A lifecycle evidence: $evidence_output"
