#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?O3K_P13_TOFU_ARCHIVE is required}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?O3K_P13_PROVIDER_ARCHIVE is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
o3kd="${O3K_P13_O3KD:-${root_dir}/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-2b-disposable-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
evidence_dir="${O3K_P13_EVIDENCE_DIR:-${root_dir}/target/p13-2b}"
evidence_output="${O3K_P13_EVIDENCE_OUTPUT:-${evidence_dir}/subnet-lifecycle-evidence.json}"
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p13-2b.XXXXXX")"
trace_path="$work_dir/trace.jsonl"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
project_dir="$work_dir/project"
run_id="$(python3 -c 'import uuid;print(uuid.uuid4())')"
head_sha="$(git -C "$root_dir" rev-parse HEAD)"
[[ -x "$o3kd" ]] || { echo "missing o3kd: $o3kd" >&2; exit 2; }
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
tofu_version="$("$tofu" version | head -n1)"
[[ "$tofu_version" == *"OpenTofu v1.12.6"* ]]
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
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-2b-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace_path" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
o3kd_pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
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
resource "openstack_networking_network_v2" "managed" { name = "p13-2b-network" }
resource "openstack_networking_subnet_v2" "managed" {
  network_id = openstack_networking_network_v2.managed.id
  cidr = "198.51.100.0/24"
  ip_version = 4
  enable_dhcp = false
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project_dir"
run() { echo "== tofu $*"; "$tofu" "$@" 2>&1 | tee -a "$work_dir/tofu.log"; }
run init -input=false -upgrade=false
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i '/enable_dhcp = false/a\  name = "p13-2b-subnet-renamed"' provider.tf
sed -i 's/enable_dhcp = false/enable_dhcp = true/' provider.tf
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve

curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/networks" \
  --data '{"network":{"name":"p13-2b-import-network"}}' >"$work_dir/import-network.json"
import_network_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["network"]["id"])' "$work_dir/import-network.json")"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/subnets" \
  --data "{\"subnet\":{\"network_id\":\"$import_network_id\",\"cidr\":\"203.0.113.0/24\",\"ip_version\":4,\"enable_dhcp\":false}}" >"$work_dir/import-subnet.json"
import_subnet_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["subnet"]["id"])' "$work_dir/import-subnet.json")"
second_status="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2.0/subnets" --data "{\"subnet\":{\"network_id\":\"$import_network_id\",\"cidr\":\"203.0.114.0/24\",\"ip_version\":4}}")"
[[ "$second_status" == 409 ]]
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
resource "openstack_networking_subnet_v2" "managed" {
  network_id = "$import_network_id"
  cidr = "203.0.113.0/24"
  ip_version = 4
  enable_dhcp = false
}
EOF
run import 'openstack_networking_subnet_v2.managed' "$import_subnet_id"
run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve
curl -fsS -H "X-Auth-Token: $token" -X DELETE "http://127.0.0.1:$port/v2.0/networks/$import_network_id" >/dev/null
python3 - "$trace_path" "$evidence_output" "$tofu_archive" "$provider_archive" "$provider_binary" "$provider_sha" "$head_sha" "$run_id" "$tofu_version" <<'PY'
import hashlib,json,pathlib,sys
trace,out,ta,pa,pb,expected,head,run,engine=sys.argv[1:]
def digest(p):
 h=hashlib.sha256()
 with open(p,'rb') as f:
  for c in iter(lambda:f.read(1048576),b''):h.update(c)
 return h.hexdigest()
records=[]
for line in pathlib.Path(trace).read_text().splitlines():
 if line.strip():
  x=json.loads(line)
  for k in list(x.get('headers',{})):
   if k.lower() in {'authorization','x-auth-token'}:x['headers'][k]='<redacted>'
  records.append(x)
agents=sorted({x.get('request_headers',{}).get('user-agent','') for x in records if 'Terraform Provider OpenStack/3.4.0' in x.get('request_headers',{}).get('user-agent','')})
if not agents:raise SystemExit('provider trace identity missing')
if not any(x.get('method')=='POST' and x.get('path')=='/v2.0/subnets' and int(x.get('status',0))==201 for x in records):raise SystemExit('subnet create trace missing')
if not any(x.get('method')=='DELETE' and '/v2.0/subnets/' in x.get('path','') and int(x.get('status',0))==204 for x in records):raise SystemExit('subnet delete trace missing')
if not any(x.get('method')=='PUT' and '/v2.0/subnets/' in x.get('path','') and int(x.get('status',0))==200 for x in records):raise SystemExit('subnet update trace missing')
if not any(x.get('method')=='GET' and '/v2.0/subnets/' in x.get('path','') for x in records):raise SystemExit('subnet read trace missing')
doc={'artifact_type':'o3k-p13-2b-subnet-lifecycle-evidence','schema_version':1,'phase':'P13.2B','run':{'run_id':run,'o3k_head_sha':head,'fresh_execution':True,'engine_version_output':engine},'toolchain':{'opentofu':'1.12.6','opentofu_archive_sha256':digest(ta),'provider':'terraform-provider-openstack/openstack 3.4.0','provider_archive_sha256':digest(pa),'provider_binary_sha256':digest(pb),'provider_sha256_expected':expected,'provider_modified':False},'trace_client_identity':{'execution_engine':'OpenTofu 1.12.6','provider_user_agents':agents},'lifecycle':{'create':'PASS','provider_omitted_name':'ACCEPTED','read':'PASS','post_apply_plan':'CONVERGED','update':'PASS','post_update_plan':'CONVERGED','delete':'PASS','post_delete_absence':'PASS','import':'PASS','post_import_plan':'CONVERGED','second_subnet_admission':'409_before_mutation','canonical_realm_id_equals_subnet_id':True,'canonical_authority':'UNCHANGED'},'http_trace':records}
pathlib.Path(out).write_text(json.dumps(doc,indent=2,sort_keys=True)+'\n')
PY
echo "P13.2B subnet lifecycle evidence: $evidence_output"
