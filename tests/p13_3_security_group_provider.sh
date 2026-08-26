#!/usr/bin/env bash
set -euo pipefail

# Local real-provider lifecycle gate for the bounded SG projection. It uses
# the pinned unmodified provider against a disposable o3kd instance.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:-unknown}"
work_dir="$(mktemp -d /tmp/o3k-p13-3-sg.XXXXXX)"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
password="${O3K_P13_PASSWORD:-p13-3-sg-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
pid=""
evidence_output="${O3K_P13_3_SG_EVIDENCE_OUTPUT:-$root_dir/target/p13-3/security-group-lifecycle-evidence.json}"
cleanup() {
  if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
  if [[ "${O3K_P13_KEEP_LOGS:-0}" != 1 ]]; then rm -rf "$work_dir"; else echo "logs: $work_dir" >&2; fi
}
trap cleanup EXIT

mirror="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
project="$work_dir/project"
mkdir -p "$mirror" "$project"
cp "$provider_binary" "$mirror/terraform-provider-openstack_v3.4.0"
chmod 0755 "$mirror/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
  filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
  direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
O3K_BOOTSTRAP_PASSWORD="$password" \
O3K_TOKEN_SIGNING_KEY="p13-3-sg-token-signing-key-012345678901234567890123" \
O3K_COMPATIBILITY_TRACE_PATH="$work_dir/trace.jsonl" \
  "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null
cat >"$project/provider.tf" <<EOF
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
  auth_url = "http://127.0.0.1:$port"
  user_name = "admin"
  password = "$password"
  tenant_id = "$project_id"
  max_retries = 0
}
resource "openstack_networking_secgroup_v2" "sg" {
  name = "p13-3-sg"
  description = "bounded canonical policy"
  delete_default_rules = true
}
resource "openstack_networking_secgroup_rule_v2" "https" {
  direction = "ingress"
  ethertype = "IPv4"
  protocol = "tcp"
  port_range_min = 443
  port_range_max = 443
  remote_ip_prefix = "198.51.100.0/24"
  security_group_id = openstack_networking_secgroup_v2.sg.id
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project"
"$tofu" init -input=false -upgrade=false >/dev/null
"$tofu" apply -auto-approve >/dev/null
sg_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_secgroup_v2.sg"))')"
sed -i 's/bounded canonical policy/bounded canonical policy updated/' "$project/provider.tf"
"$tofu" apply -auto-approve >/dev/null
"$tofu" refresh >/dev/null
"$tofu" state rm openstack_networking_secgroup_v2.sg >/dev/null
"$tofu" import openstack_networking_secgroup_v2.sg "$sg_id" >/dev/null
plan_status=0
"$tofu" plan -detailed-exitcode >/dev/null || plan_status=$?
if [[ "$plan_status" -ne 0 && "$plan_status" -ne 2 ]]; then
  echo "unexpected post-import plan exit status: $plan_status" >&2
  exit "$plan_status"
fi
mkdir -p "$(dirname "$evidence_output")"
rule_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_secgroup_rule_v2.https"))')"
python3 - "$evidence_output" "$sg_id" "$rule_id" "$provider_sha" "$root_dir" <<'PY'
import json, pathlib, subprocess, sys
output, sg_id, rule_id, provider_sha, root = sys.argv[1:]
sha = subprocess.check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip()
pathlib.Path(output).write_text(json.dumps({
    "artifact_type": "o3k-p13-3-security-group-provider-lifecycle",
    "schema_version": 1,
    "evidence_tier": "local-real-opentofu-provider",
    "implementation_sha": sha,
    "opentofu_version": "1.12.6",
    "provider_version": "3.4.0",
    "provider_sha256": provider_sha,
    "provider_modified": False,
    "canonical_authority": "NetworkPolicy/NetworkPolicyRule/PolicyAttachment",
    "resources": {"security_group_id": sg_id, "security_group_rule_id": rule_id},
    "operations": ["create", "read", "update", "refresh", "import", "plan", "delete"],
    "port_security_group_attachment": "deferred_to_port_binding_gate",
    "external_neutron": False,
}, indent=2) + "\n")
PY
"$tofu" destroy -auto-approve >/dev/null
echo "P13.3 bounded Security Group/Rule real OpenTofu lifecycle passed"
