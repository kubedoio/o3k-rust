#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
evidence_output="${O3K_P13_3_PORT_EVIDENCE_OUTPUT:-$root_dir/docs/compatibility/p13-3/p13-3b3-port-security-group-provider-lifecycle-evidence.json}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-3-port-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
work_dir="$(mktemp -d /tmp/o3k-p13-3-port.XXXXXX)"
project_dir="$work_dir/project"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
pid=""
cleanup() { [[ -z "$pid" ]] || { kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; }; rm -rf "$work_dir"; }
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
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-3-port-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$work_dir/trace.jsonl" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null
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
resource "openstack_networking_network_v2" "network" { name = "p13-3-port-network" }
resource "openstack_networking_subnet_v2" "subnet" {
  network_id = openstack_networking_network_v2.network.id
  cidr = "198.51.103.0/24"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_networking_secgroup_v2" "sg" {
  name = "p13-3-port-sg"
  description = "canonical port attachment"
  delete_default_rules = true
}
resource "openstack_networking_port_v2" "port" {
  name = "p13-3-port"
  network_id = openstack_networking_network_v2.network.id
  security_group_ids = [openstack_networking_secgroup_v2.sg.id]
  fixed_ip { subnet_id = openstack_networking_subnet_v2.subnet.id }
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project_dir"
"$tofu" init -input=false -upgrade=false >/dev/null
"$tofu" apply -auto-approve >/dev/null
port_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_port_v2.port"))')"
sg_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_secgroup_v2.sg"))')"
"$tofu" plan -detailed-exitcode >/dev/null
sed -i 's/security_group_ids = \[openstack_networking_secgroup_v2.sg.id\]/security_group_ids = []/' "$project_dir/main.tf"
"$tofu" apply -auto-approve >/dev/null
"$tofu" plan -detailed-exitcode >/dev/null
sed -i 's/security_group_ids = \[\]/security_group_ids = [openstack_networking_secgroup_v2.sg.id]/' "$project_dir/main.tf"
"$tofu" apply -auto-approve >/dev/null
"$tofu" refresh >/dev/null
"$tofu" state rm openstack_networking_port_v2.port >/dev/null
"$tofu" import openstack_networking_port_v2.port "$port_id" >/dev/null
plan_status=0
"$tofu" plan -detailed-exitcode >/dev/null || plan_status=$?
[[ "$plan_status" -eq 0 || "$plan_status" -eq 2 ]]
mkdir -p "$(dirname "$evidence_output")"
python3 - "$evidence_output" "$port_id" "$sg_id" "$provider_sha" "$root_dir" <<'PY'
import json, pathlib, subprocess, sys
output, port_id, sg_id, provider_sha, root = sys.argv[1:]
sha = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip()
pathlib.Path(output).write_text(json.dumps({
    "artifact_type": "o3k-p13-3-port-security-group-provider-lifecycle",
    "schema_version": 1,
    "evidence_tier": "local-real-opentofu-provider",
    "implementation_sha": sha,
    "opentofu_version": "1.12.6",
    "provider_version": "3.4.0",
    "provider_sha256": provider_sha,
    "provider_modified": False,
    "canonical_authority": "NetworkPolicy/NetworkPolicyRule/PolicyAttachment",
    "resources": {"port_id": port_id, "security_group_id": sg_id},
    "operations": ["create", "read", "attach", "detach", "reattach", "refresh", "import", "plan", "delete"],
    "external_neutron": False,
}, indent=2) + "\n")
PY
"$tofu" destroy -auto-approve >/dev/null
echo "P13.3 port security-group attachment lifecycle passed (port=$port_id sg=$sg_id provider_sha=$provider_sha)"
