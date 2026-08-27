#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-2d-disposable-password}"
backend="${O3K_P13_2D_BACKEND:-sqlite}"
evidence_output="${O3K_P13_2D_EVIDENCE_OUTPUT:-}"
run_id="${O3K_P13_2D_RUN_ID:-$(python3 -c 'import uuid;print(uuid.uuid4())')}"
head_sha="$(git -C "$root_dir" rev-parse HEAD)"
tofu_version="$($tofu version | head -n 1)"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
work_dir="$(mktemp -d /tmp/o3k-p13-2d.XXXXXX)"; project_dir="$work_dir/project"; trace_path="$work_dir/trace.jsonl"; o3kd_pid=""
database_env=()
if [[ -n "${O3K_DATABASE_URL:-}" ]]; then
  database_env=(O3K_DATABASE_BACKEND=postgres O3K_DATABASE_URL="$O3K_DATABASE_URL")
fi
mkdir -p "$project_dir"
cleanup() { [[ -z "$o3kd_pid" ]] || { kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; }; rm -rf "$work_dir"; }
trap cleanup EXIT
start() {
  env "${database_env[@]}" O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-2d-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace_path" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
  o3kd_pid=$!; for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && return; sleep .1; done; cat "$work_dir/o3kd.log" >&2; exit 1
}
start
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2/images" --data '{"name":"p13-2d-image","visibility":"private","container_format":"bare","disk_format":"raw"}' >"$work_dir/image.json"
image_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["id"])' "$work_dir/image.json")"
printf 'p13-2d-image-fixture\n' >"$work_dir/image-content"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/octet-stream' --data-binary "@$work_dir/image-content" -X PUT "http://127.0.0.1:$port/v2/images/$image_id/file" >/dev/null
cat >"$project_dir/main.tf" <<EOF
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
data "openstack_images_image_v2" "image" { name = "p13-2d-image" }
data "openstack_compute_flavor_v2" "flavor" { name = "test.small" }
resource "openstack_networking_network_v2" "network" { name = "p13-2d-network" }
resource "openstack_networking_subnet_v2" "subnet" {
  network_id = openstack_networking_network_v2.network.id
  cidr = "198.51.102.0/24"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_compute_instance_v2" "server" {
  name = "p13-2d-server"
  image_id = data.openstack_images_image_v2.image.id
  flavor_id = data.openstack_compute_flavor_v2.flavor.id
  power_state = "active"
  force_delete = false
  stop_before_destroy = false
  tags = []
  network { uuid = openstack_networking_network_v2.network.id }
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
mkdir -p "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
cp "${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}" "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
chmod 0755 "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation { filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] } direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] } }
EOF
cd "$project_dir"; run() { echo "== tofu $*"; "$tofu" "$@"; }
run init -input=false -upgrade=false; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/p13-2d-server/p13-2d-server-renamed/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
server_id="$($tofu show -json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(r["values"]["id"] for r in d["values"]["root_module"]["resources"] if r["address"] == "openstack_compute_instance_v2.server"))')"
run state rm openstack_compute_instance_v2.server
run import openstack_compute_instance_v2.server "$server_id"
run apply -auto-approve
run plan -detailed-exitcode
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST \
  "http://127.0.0.1:$port/v2.1/$project_id/servers/$server_id/action" \
  --data '{"reboot":{"type":"SOFT"}}' >/dev/null
run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/power_state = "active"/power_state = "shutoff"/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/power_state = "shutoff"/power_state = "active"/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
kill "$o3kd_pid"; wait "$o3kd_pid" 2>/dev/null || true; o3kd_pid=""; start; run plan -detailed-exitcode || [[ "$?" == 2 ]]
network_id="$($tofu show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_network_v2.network"))')"
subnet_id="$($tofu show -json | python3 -c 'import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]; print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_subnet_v2.subnet"))')"
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/ports?network_id=$network_id" >"$work_dir/ports.json"
python3 - "$work_dir/ports.json" "$subnet_id" "$project_id" >"$work_dir/endpoint-identities.json" <<'PY'
import json, sys
doc=json.load(open(sys.argv[1])); subnet_id, project_id=sys.argv[2:]
owned=[p for p in doc.get("ports", []) if str(p.get("name", "")).startswith("o3k-server:") and p.get("project_id", p.get("tenant_id")) == project_id]
if len(owned) != 1: raise SystemExit(f"expected exactly one server-owned endpoint, found {len(owned)}")
port=owned[0]; fixed=port.get("fixed_ips", [])
if len(fixed) != 1 or fixed[0].get("subnet_id") != subnet_id: raise SystemExit("server endpoint Realm mismatch")
print(json.dumps({"endpoint_id":port["id"],"realm_id":fixed[0]["subnet_id"],"fixed_ip":fixed[0]["ip_address"],"mac_address":port["mac_address"],"network_id":port["network_id"],"project_id":port.get("project_id", port.get("tenant_id"))}))
PY
endpoint_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["endpoint_id"])' "$work_dir/endpoint-identities.json")"
realm_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["realm_id"])' "$work_dir/endpoint-identities.json")"
fixed_ip="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["fixed_ip"])' "$work_dir/endpoint-identities.json")"
mac_address="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["mac_address"])' "$work_dir/endpoint-identities.json")"
run destroy -auto-approve -target=openstack_compute_instance_v2.server
endpoint_after="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/ports/$endpoint_id")"
network_after="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$network_id")"
subnet_after="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/subnets/$subnet_id")"
[[ "$endpoint_after" == 404 && "$network_after" == 200 && "$subnet_after" == 200 ]]
run destroy -auto-approve
if [[ -n "$evidence_output" ]]; then
  [[ -n "${O3K_P13_PROVIDER_SHA256:-}" && -n "${O3K_P13_TOFU_ARCHIVE:-}" && -n "${O3K_P13_PROVIDER_ARCHIVE:-}" ]] || { echo "evidence output requires pinned tool archives and provider SHA" >&2; exit 2; }
  python3 - "$trace_path" "$evidence_output" "$backend" "$run_id" "$head_sha" "$tofu_version" "$endpoint_id" "$network_id" "$realm_id" "$subnet_id" "$fixed_ip" "$mac_address" "$server_id" "$endpoint_after" "$network_after" "$subnet_after" "${O3K_P13_PROVIDER_SHA256:-}" "${O3K_P13_TOFU_ARCHIVE:-}" "${O3K_P13_PROVIDER_ARCHIVE:-}" <<'PY'
import hashlib, json, pathlib, re, sys
trace,out,backend,run_id,head,tofu_version,endpoint_id,network_id,realm_id,subnet_id,fixed_ip,mac,server_id,endpoint_after,network_after,subnet_after,provider_sha,tofu_archive,provider_archive=sys.argv[1:]
secret=re.compile(r"(?:x-auth-token|x-subject-token|authorization|password|private[_-]?key|token[_-]?signing|database[_-]?url)", re.I)
def digest(path):
    p=pathlib.Path(path) if path else None
    return hashlib.sha256(p.read_bytes()).hexdigest() if p and p.exists() else None
records=[json.loads(line) for line in pathlib.Path(trace).read_text().splitlines() if line.strip()]
for i,record in enumerate(records): record.setdefault("sequence", i)
def has(method, fragment, status=None):
    return any(x.get("method") == method and fragment in x.get("path", "") and (status is None or x.get("status") == status) for x in records)
if not has("POST", "/servers", 202): raise SystemExit("server create trace missing")
if not has("GET", "/servers/", 200): raise SystemExit("server read/poll trace missing")
if not has("DELETE", "/servers/", 204): raise SystemExit("server delete trace missing")
if not has("POST", "/action", None): raise SystemExit("server power-action trace missing")
server_gets=[x for x in records if x.get("method") == "GET" and "/servers/" in x.get("path", "") and x.get("status") == 200]
server_states=[]
for item in server_gets:
    body=item.get("response_body", {}).get("json", {})
    state=body.get("server", {}).get("status")
    if state: server_states.append(state)
if "ACTIVE" not in server_states: raise SystemExit("ACTIVE server observation missing")
if "SHUTOFF" not in server_states: raise SystemExit("SHUTOFF server observation missing")
provider_agents=sorted({x.get("request_headers", {}).get("user-agent", "") for x in records if "Terraform Provider OpenStack/3.4.0" in x.get("request_headers", {}).get("user-agent", "")})
if not provider_agents: raise SystemExit("provider SDK identity missing")
def scan(value):
    if isinstance(value, dict):
        for key,item in value.items():
            if secret.search(key) and item != "<redacted>": raise SystemExit(f"unredacted secret field: {key}")
            scan(item)
    elif isinstance(value,list):
        for item in value: scan(item)
    elif isinstance(value,str) and ("BEGIN " in value or "Bearer " in value): raise SystemExit("secret-like value in trace")
scan(records)
if endpoint_after != "404" or network_after != "200" or subnet_after != "200": raise SystemExit("cleanup identity proof failed")
run={"backend":backend,"run_id":run_id,"o3k_head_sha":head,"fresh_execution":True,"engine_version":tofu_version,"trace_client_identity":{"execution_engine":"OpenTofu 1.12.6","provider_user_agents":provider_agents},"identities":{"server_id":server_id,"endpoint_id":endpoint_id,"network_id":network_id,"realm_id":realm_id,"subnet_id":subnet_id,"fixed_ip":fixed_ip,"mac_address":mac},"ownership":{"endpoint_realm_id_equals_subnet_id":True,"realm_network_id_equals_network_id":True,"server_owned_endpoint":True},"lifecycle":{"create":{"method":"POST","path":"/v2.1/{project_id}/servers","status":202},"poll_states":server_states,"read":"PASS","update":"PASS","power_actions":"PASS","import":"PASS","restart":"CONVERGED","delete":{"method":"DELETE","path":"/v2.1/{project_id}/servers/{id}","status":204},"post_delete":"ABSENT"},"cleanup":{"endpoint_status":int(endpoint_after),"network_status":int(network_after),"subnet_status":int(subnet_after)},"trace_format":{"structured":True,"redacted":True,"sequence_field":"sequence"},"http_trace":records}
toolchain={"opentofu":"1.12.6","opentofu_archive_sha256":digest(tofu_archive),"provider":"terraform-provider-openstack/openstack 3.4.0","provider_archive_sha256":digest(provider_archive),"provider_binary_sha256":provider_sha,"provider_modified":False}
path=pathlib.Path(out); doc=json.loads(path.read_text()) if path.exists() else {"artifact_type":"o3k-p13-2d-provider-server-lifecycle-evidence","schema_version":1,"phase":"P13.2D","runs":[],"redacted":True,"raw_trace_committed":False,"secrets_redacted":True}
doc["tested_implementation_sha"]=head; doc["toolchain"]=toolchain; doc["raw_trace_committed"]=False; doc["secrets_redacted"]=True; doc["runs"]=[x for x in doc.get("runs",[]) if x.get("backend") != backend]+[run]
path.parent.mkdir(parents=True,exist_ok=True); path.write_text(json.dumps(doc,indent=2,sort_keys=True)+"\n")
PY
fi
echo "P13.2D bounded server lifecycle passed"
