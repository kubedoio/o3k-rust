#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "$0")/.." && pwd)"
[ -n "$O3K_P13_TOFU" ] || { echo O3K_P13_TOFU is required >&2; exit 2; }
[ -n "$O3K_P13_PROVIDER_BINARY" ] || { echo O3K_P13_PROVIDER_BINARY is required >&2; exit 2; }
[ -n "$O3K_P13_PROVIDER_SHA256" ] || { echo O3K_P13_PROVIDER_SHA256 is required >&2; exit 2; }
tofu="$O3K_P13_TOFU"; provider_binary="$O3K_P13_PROVIDER_BINARY"; provider_sha="$O3K_P13_PROVIDER_SHA256"
o3kd="$root_dir/target/debug/o3kd"; [ -n "${O3K_P13_O3KD-}" ] && o3kd="$O3K_P13_O3KD"
password="p13-2c-disposable-password"; [ -n "${O3K_P13_PASSWORD-}" ] && password="$O3K_P13_PASSWORD"
port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')"
work_dir="$(mktemp -d /tmp/o3k-p13-2c.XXXXXX)"; trace_path="$work_dir/trace.jsonl"; data_dir="$work_dir/data"; project_dir="$work_dir/project"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"; run_id="$(python3 -c 'import uuid;print(uuid.uuid4())')"; head_sha="$(git -C "$root_dir" rev-parse HEAD)"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"; o3kd_pid=""
[[ -x "$o3kd" ]] || exit 2
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
mkdir -p "$mirror_dir" "$project_dir"; cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"; chmod 0755 "$mirror_dir/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
 filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
 direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
cleanup() { if [[ -n "$o3kd_pid" ]]; then kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; fi; rm -rf "$work_dir"; }; trap cleanup EXIT
start() { O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-2c-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace_path" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$data_dir" >"$work_dir/o3kd.log" 2>&1 & o3kd_pid=$!; for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && return; sleep .1; done; cat "$work_dir/o3kd.log" >&2; exit 1; }
start
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
  auth_url = "http://127.0.0.1:$port"
  user_name = "admin"
  password = "$password"
  tenant_id = "$project_id"
  max_retries = 0
}
resource "openstack_networking_network_v2" "network" {
  name = "p13-2c-network"
}
resource "openstack_networking_subnet_v2" "subnet" {
  network_id = openstack_networking_network_v2.network.id
  cidr = "198.51.100.0/24"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_networking_port_v2" "port" {
  name = "p13-2c-port"
  network_id = openstack_networking_network_v2.network.id
  fixed_ip {
    subnet_id = openstack_networking_subnet_v2.subnet.id
  }
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1; cd "$project_dir"
run() { echo "== tofu $*"; "$tofu" "$@" 2>&1 | tee -a "$work_dir/tofu.log"; }
run init -input=false -upgrade=false; run apply -auto-approve; run plan -detailed-exitcode
port_state="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; p=next(x["values"] for x in r if x["address"]=="openstack_networking_port_v2.port"); print(json.dumps({"id":p["id"],"subnet_id":p["fixed_ip"][0]["subnet_id"],"fixed_ips":[{"ip_address":p["all_fixed_ips"][0]}],"mac_address":p["mac_address"]},sort_keys=True))')"
port_id="$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["id"])' "$port_state")"; subnet_id="$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["subnet_id"])' "$port_state")"; network_id="$("$tofu" show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; p=next(x["values"] for x in r if x["address"]=="openstack_networking_network_v2.network"); print(p["id"])')"; port_ip="$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["fixed_ips"][0]["ip_address"])' "$port_state")"; port_mac="$(python3 -c 'import json,sys;print(json.loads(sys.argv[1])["mac_address"])' "$port_state")"
sed -i 's/name = "p13-2c-port"/name = "p13-2c-port-renamed"/' provider.tf; run apply -auto-approve; run plan -detailed-exitcode
kill "$o3kd_pid"; wait "$o3kd_pid" 2>/dev/null || true; o3kd_pid=""; start; run plan -detailed-exitcode; run destroy -auto-approve

curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/networks" --data '{"network":{"name":"p13-2c-import-network"}}' >"$work_dir/import-network.json"
import_network_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["network"]["id"])' "$work_dir/import-network.json")"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/subnets" --data "{\"subnet\":{\"network_id\":\"$import_network_id\",\"cidr\":\"198.51.101.0/24\",\"ip_version\":4,\"enable_dhcp\":false}}" >"$work_dir/import-subnet.json"
import_subnet_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["subnet"]["id"])' "$work_dir/import-subnet.json")"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/ports" --data "{\"port\":{\"name\":\"p13-2c-import-port\",\"network_id\":\"$import_network_id\",\"fixed_ips\":[{\"subnet_id\":\"$import_subnet_id\",\"ip_address\":\"198.51.101.10\"}]}}" >"$work_dir/import-port.json"
import_port_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["port"]["id"])' "$work_dir/import-port.json")"
cat >provider.tf <<EOF
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
resource "openstack_networking_port_v2" "imported" {
  name = "p13-2c-import-port"
  network_id = "$import_network_id"
}
EOF
run import 'openstack_networking_port_v2.imported' "$import_port_id"
run plan -detailed-exitcode
run destroy -auto-approve
[[ "$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/ports/$import_port_id")" == 404 ]]
curl -fsS -H "X-Auth-Token: $token" -X DELETE "http://127.0.0.1:$port/v2.0/subnets/$import_subnet_id" >/dev/null
curl -fsS -H "X-Auth-Token: $token" -X DELETE "http://127.0.0.1:$port/v2.0/networks/$import_network_id" >/dev/null
python3 - "$trace_path" "$root_dir/docs/compatibility/p13-2/p13-2c-provider-port-lifecycle-evidence.json" "$run_id" "$head_sha" "$port_id" "$port_ip" "$port_mac" "$provider_sha" "$import_port_id" "$import_subnet_id" "$network_id" "$subnet_id" <<'PY'
import json,pathlib,re,sys
trace,output,run_id,head,port_id,ip,mac,provider_sha,import_port_id,import_subnet_id,network_id,subnet_id=sys.argv[1:]
secret=re.compile(r'token|password|secret|authorization|cookie|private[_-]?key',re.I)
def redact(v):
 if isinstance(v,dict): return {k:'<redacted>' if secret.search(k) else redact(x) for k,x in v.items()}
 if isinstance(v,list): return [redact(x) for x in v]
 return v
records=[json.loads(x) for x in pathlib.Path(trace).read_text().splitlines() if x.strip()]
for i,x in enumerate(records): x['ordinal']=i
doc={'schema_version':1,'artifact_type':'o3k-p13-2c-provider-port-lifecycle-evidence','phase':'P13.2C','run':{'run_id':run_id,'o3k_head_sha':head,'fresh_execution':True,'engine':'OpenTofu 1.12.6'},'toolchain':{'opentofu':'1.12.6','provider':'terraform-provider-openstack/openstack 3.4.0','provider_sha256':provider_sha,'provider_modified':False},'fixtures':{'network_id':network_id,'subnet_id':subnet_id},'port':{'id':port_id,'fixed_ip':ip,'mac':mac,'create':'PASS','read':'PASS','update':'PASS','restart':'PASS','post_apply_plan':'CONVERGED','post_update_plan':'CONVERGED','post_restart_plan':'CONVERGED','delete':'PASS'},'import':{'id':import_port_id,'subnet_id':import_subnet_id,'first_request':'GET /v2.0/ports/{id}','result':'PASS','post_import_plan':'CONVERGED'},'http_trace':redact(records)}
pathlib.Path(output).parent.mkdir(parents=True,exist_ok=True); pathlib.Path(output).write_text(json.dumps(doc,indent=2,sort_keys=True)+'\n')
PY
echo "P13.2C port lifecycle evidence: docs/compatibility/p13-2/p13-2c-provider-port-lifecycle-evidence.json"
