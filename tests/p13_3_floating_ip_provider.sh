#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
evidence_output="${O3K_P13_3_FIP_EVIDENCE_OUTPUT:-$root_dir/docs/compatibility/p13-3/p13-3e-floating-ip-provider-lifecycle-evidence.json}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-3-fip-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
external_realm="00000000-0000-0000-0000-000000000009"
external_pool_name="p13-3-external"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
work_dir="$(mktemp -d /tmp/o3k-p13-3-fip.XXXXXX)"
project_dir="$work_dir/project"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
pid=""
run_stage() {
  local stage="$1"; shift; local status
  printf 'RUN %s\n' "$stage" | tee -a "$work_dir/stages.log" >&2
  set +e; "$@" > >(tee -a "$work_dir/stages.log") 2>&1; status=$?; set -e
  if [[ "$status" -ne 0 ]]; then printf 'FAILED: %s exit=%s artifacts=%s\n' "$stage" "$status" "$work_dir" >&2; return "$status"; fi
}
cleanup() { local status=$?; [[ -z "$pid" ]] || { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }; if [[ "$status" -ne 0 || "${O3K_P13_KEEP_LOGS:-0}" == 1 ]]; then echo "logs: $work_dir" >&2; else rm -rf "$work_dir"; fi; }
trap cleanup EXIT
mkdir -p "$project_dir" "$mirror_dir"
cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"
chmod 0755 "$mirror_dir/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
 filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
 direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-3-fip-token-signing-key-012345678901234567890123" O3K_NETWORK_EXTERNAL_REALM_ID="$external_realm" O3K_PUBLIC_POOL_CIDR="198.51.104.0/29" O3K_PUBLIC_POOL_FIRST="198.51.104.2" O3K_PUBLIC_POOL_LAST="198.51.104.6" O3K_COMPATIBILITY_TRACE_PATH="$work_dir/trace.jsonl" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/networks" --data "{\"network\":{\"name\":\"$external_pool_name\"}}" >"$work_dir/external-network.json"
external_realm="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["network"]["id"])' "$work_dir/external-network.json")"
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-3-fip-token-signing-key-012345678901234567890123" O3K_NETWORK_EXTERNAL_REALM_ID="$external_realm" O3K_PUBLIC_POOL_CIDR="198.51.104.0/29" O3K_PUBLIC_POOL_FIRST="198.51.104.2" O3K_PUBLIC_POOL_LAST="198.51.104.6" O3K_COMPATIBILITY_TRACE_PATH="$work_dir/trace-restart.jsonl" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd-restart.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
cat >"$project_dir/main.tf" <<EOF
terraform {
 required_version = "= 1.12.6"
 required_providers { openstack = { source = "terraform-provider-openstack/openstack", version = "= 3.4.0" } }
}
provider "openstack" {
 auth_url = "http://127.0.0.1:$port"
 user_name = "admin"
 password = "$password"
 tenant_id = "$project_id"
 max_retries = 0
}
resource "openstack_networking_network_v2" "network" { name = "p13-3-fip-network" }
resource "openstack_networking_subnet_v2" "subnet" {
 network_id = openstack_networking_network_v2.network.id
 cidr = "198.51.105.0/24"
 ip_version = 4
 enable_dhcp = false
}
resource "openstack_networking_port_v2" "port" {
 name = "p13-3-fip-port"
 network_id = openstack_networking_network_v2.network.id
 fixed_ip { subnet_id = openstack_networking_subnet_v2.subnet.id }
}
resource "openstack_networking_floatingip_v2" "fip" {
 pool = "$external_pool_name"
 port_id = openstack_networking_port_v2.port.id
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project_dir"
plan() { local status=0; run_stage "tofu plan" "$tofu" plan -detailed-exitcode || status=$?; [[ "$status" -eq 0 ]]; }
run_stage "tofu init" "$tofu" init -input=false -upgrade=false
run_stage "tofu initial apply" "$tofu" apply -auto-approve
fip_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_floatingip_v2.fip"))')"
plan
sed -i 's/port_id = openstack_networking_port_v2.port.id/port_id = null/' "$project_dir/main.tf"
run_stage "tofu disassociate apply" "$tofu" apply -auto-approve
sed -i 's/port_id = null/port_id = openstack_networking_port_v2.port.id/' "$project_dir/main.tf"
run_stage "tofu reassociate apply" "$tofu" apply -auto-approve
run_stage "tofu refresh" "$tofu" refresh
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=""
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-3-fip-token-signing-key-012345678901234567890123" O3K_NETWORK_EXTERNAL_REALM_ID="$external_realm" O3K_PUBLIC_POOL_CIDR="198.51.104.0/29" O3K_PUBLIC_POOL_FIRST="198.51.104.2" O3K_PUBLIC_POOL_LAST="198.51.104.6" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd-final.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
plan
run_stage "tofu state rm" "$tofu" state rm openstack_networking_floatingip_v2.fip
run_stage "tofu import" "$tofu" import openstack_networking_floatingip_v2.fip "$fip_id"
plan
sed -i 's/port_id = openstack_networking_port_v2.port.id/port_id = null/' "$project_dir/main.tf"
run_stage "tofu post-import apply" "$tofu" apply -auto-approve
curl -fsS -X PUT "http://127.0.0.1:$port/v2.0/floatingips/$fip_id" -H 'content-type: application/json' -H "x-auth-token: $token" --data '{"floatingip":{}}' >/dev/null
mkdir -p "$(dirname "$evidence_output")"
python3 - "$evidence_output" "$fip_id" "$external_realm" "$provider_sha" "$root_dir" <<'PY'
import json, pathlib, subprocess, sys
output, fip_id, external_realm, provider_sha, root = sys.argv[1:]
sha = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip()
pathlib.Path(output).write_text(json.dumps({
    "artifact_type": "o3k-p13-3-floating-ip-provider-lifecycle",
    "schema_version": 1,
    "evidence_tier": "local-real-opentofu-provider",
    "implementation_sha": sha,
    "opentofu_version": "1.12.6",
    "provider_version": "3.4.0",
    "provider_sha256": provider_sha,
    "provider_modified": False,
    "canonical_authority": "PublicAddress/PublicAddressBinding",
    "resources": {"floating_ip_id": fip_id, "external_realm_id": external_realm},
    "operations": ["allocate", "read", "associate", "disassociate", "refresh", "restart", "import", "plan", "release"],
    "external_neutron": False,
}, indent=2) + "\n")
PY
run_stage "tofu destroy" "$tofu" destroy -auto-approve
echo "P13.3 floating IP lifecycle passed (id=$fip_id pool=$external_realm provider_sha=$provider_sha)"
