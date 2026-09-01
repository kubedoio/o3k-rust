#!/usr/bin/env bash
set -euo pipefail

# External-process P13.5C native DELETE proof. No state or canonical-store
# shortcuts are used; the only mutation is the public native DELETE.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?O3K_P13_TOFU_ARCHIVE is required}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?O3K_P13_PROVIDER_ARCHIVE is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
baseline="${P13_5B_BASELINE_MANIFEST:?P13_5B_BASELINE_MANIFEST is required}"
output="${O3K_P13_5C_EVIDENCE_OUTPUT:-$root_dir/target/p13-5c/native-drift-evidence.json}"
[[ "$output" = /* ]] || output="$root_dir/$output"
if [[ -n "${O3K_P13_5C_WORK_DIR:-}" ]]; then
  work_dir="$O3K_P13_5C_WORK_DIR"
  mkdir -p "$work_dir"
  [[ -z "$(find "$work_dir" -mindepth 1 -maxdepth 1 -print -quit)" ]] || {
    echo "O3K_P13_5C_WORK_DIR must be empty: $work_dir" >&2
    exit 1
  }
else
  work_dir="$(mktemp -d /var/tmp/o3k-p13-5c-native.XXXXXX)"
fi
mkdir -p "$(dirname "$output")" "$work_dir"
project_id=eba29e2d-53de-461d-ae91-ede7402713cb
password="${O3K_P13_PASSWORD:-p13-5c-disposable-password}"
port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
head_sha="$(git -C "$root_dir" rev-parse HEAD)"
run_id="$(python3 -c 'import uuid;print(uuid.uuid4())')"
mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
project_dir="$work_dir/project"
mkdir -p "$mirror_dir" "$project_dir"

python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
tofu_version="$($tofu version | head -n1)"
[[ "$tofu_version" == *"OpenTofu v1.12.6"* ]] || { echo "wrong OpenTofu: $tofu_version" >&2; exit 1; }
python3 - "$baseline" "$head_sha" <<'PY'
import json,sys
d=json.load(open(sys.argv[1]))
if d.get("status") != "verified" or d.get("source_commit") != sys.argv[2]: raise SystemExit("baseline is not verified for this HEAD")
PY
cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"
chmod 0755 "$mirror_dir/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
  filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
  direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
cleanup() {
  if [[ -n "${o3kd_pid:-}" ]]; then kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; fi
  if [[ -f "$project_dir/provider.tf" ]]; then
    python3 - "$project_dir/provider.tf" "$password" <<'PY'
import pathlib, sys
path = pathlib.Path(sys.argv[1])
secret = sys.argv[2]
path.write_text(path.read_text().replace(secret, "[REDACTED]"))
PY
  fi
}
trap cleanup EXIT

trace="$work_dir/compatibility-trace.jsonl"
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-5c-native-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace" \
  "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
o3kd_pid=$!
for _ in $(seq 1 180); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break; sleep .1; done
curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
[[ -n "$token" ]] || { echo "authentication did not return a token" >&2; exit 1; }

cat >"$project_dir/provider.tf" <<EOF
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
resource "openstack_networking_network_v2" "managed" {
  name = "p13-5c-native-network"
  admin_state_up = true
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
cd "$project_dir"
run_tofu() { "$tofu" "$@" 2>&1 | tee -a "$work_dir/tofu.log"; }
run_tofu init -input=false -upgrade=false
run_tofu apply -auto-approve
run_tofu show -json >"$work_dir/state-before.json"
network_id="$(python3 - "$work_dir/state-before.json" <<'PY'
import json,sys
for r in json.load(open(sys.argv[1]))["values"]["root_module"]["resources"]:
 if r["address"]=="openstack_networking_network_v2.managed": print(r["values"]["id"]); break
else: raise SystemExit("network missing from state")
PY
)"
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks/$network_id" >"$work_dir/native-before.json"
owner_scope="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["metadata"]["owner_scope"])' "$work_dir/native-before.json")"
generation="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["metadata"]["generation"])' "$work_dir/native-before.json")"
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/resource-types" >"$work_dir/resource-types.json"
python3 - "$work_dir/resource-types.json" <<'PY'
import json,sys
m=[x for x in json.load(open(sys.argv[1]))["resource_types"] if x["namespace"]=="network" and x["collection"]=="networks"]
if len(m)!=1 or not m[0]["ready"] or "delete" not in m[0]["lifecycle_actions"]: raise SystemExit("Network native DELETE is not Ready")
PY
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks" >"$work_dir/native-list-before.json"
delete_key="p13-5c-network-delete-$run_id"
delete_headers="$work_dir/delete.headers"; delete_body="$work_dir/delete.body"
delete_status="$(curl -sS -D "$delete_headers" -o "$delete_body" -w '%{http_code}' -H "Authorization: Bearer $token" -H "Idempotency-Key: $delete_key" -H "If-Match: generation-$generation" -X DELETE "http://127.0.0.1:$port/o3k/v1/network/networks/$network_id")"
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks" >"$work_dir/native-list-after-delete.json"
replay_headers="$work_dir/replay.headers"; replay_body="$work_dir/replay.body"
replay_status="$(curl -sS -D "$replay_headers" -o "$replay_body" -w '%{http_code}' -H "Authorization: Bearer $token" -H "Idempotency-Key: $delete_key" -H "If-Match: generation-$generation" -X DELETE "http://127.0.0.1:$port/o3k/v1/network/networks/$network_id")"
[[ "$delete_status" == 204 && "$replay_status" == 204 ]] || { echo "native delete/replay failed: $delete_status/$replay_status" >&2; exit 1; }
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks" >"$work_dir/native-list-after-replay.json"
native_absence_status="$(curl -sS -o "$work_dir/native-absence.body" -w '%{http_code}' -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks/$network_id")"
printf '%s\n' "$native_absence_status" >"$work_dir/native-absence.status"
[[ "$native_absence_status" == 404 ]] || { echo "native resource did not observe absence: $native_absence_status" >&2; exit 1; }
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks" >"$work_dir/native-list-after-delete.json"
compat_absence="$(curl -sS -o "$work_dir/compat-absence.body" -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$network_id")"
[[ "$compat_absence" == 404 ]] || { echo "compatibility did not observe absence: $compat_absence" >&2; exit 1; }
set +e
run_tofu plan -refresh-only -detailed-exitcode -out="$work_dir/refresh.plan" -input=false
refresh_exit=$?
run_tofu plan -detailed-exitcode -out="$work_dir/normal.plan" -input=false
normal_exit=$?
set -e
[[ "$refresh_exit" == 2 && "$normal_exit" == 2 ]] || { echo "plans did not report exact recreation: refresh=$refresh_exit normal=$normal_exit" >&2; exit 1; }
run_tofu show -json "$work_dir/refresh.plan" >"$work_dir/refresh.json"
run_tofu show -json "$work_dir/normal.plan" >"$work_dir/normal.json"
run_tofu apply -auto-approve
run_tofu show -json >"$work_dir/state-after.json"
replacement_id="$(python3 - "$work_dir/state-after.json" <<'PY'
import json,sys
for r in json.load(open(sys.argv[1]))["values"]["root_module"]["resources"]:
 if r["address"]=="openstack_networking_network_v2.managed": print(r["values"]["id"]); break
else: raise SystemExit("replacement missing from state")
PY
)"
[[ "$replacement_id" != "$network_id" ]] || { echo "replacement reused old ID" >&2; exit 1; }
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks/$replacement_id" >"$work_dir/native-after.json"
replacement_scope="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["metadata"]["owner_scope"])' "$work_dir/native-after.json")"
[[ "$replacement_scope" == "$owner_scope" ]] || { echo "replacement owner scope changed" >&2; exit 1; }
curl -fsS -H "Authorization: Bearer $token" "http://127.0.0.1:$port/o3k/v1/network/networks" >"$work_dir/native-list-after.json"
final_exit=0; run_tofu plan -detailed-exitcode -input=false >/dev/null || final_exit=$?
[[ "$final_exit" == 0 ]] || { echo "final plan is not no-op" >&2; exit 1; }
delete_key_digest="$(printf '%s' "$delete_key" | sha256sum | awk '{print $1}')"

python3 - "$root_dir" "$output" "$baseline" "$head_sha" "$run_id" "$tofu_version" "$tofu_archive" "$provider_archive" "$provider_binary" "$provider_sha" "$project_id" "$network_id" "$replacement_id" "$owner_scope" "$replacement_scope" "$delete_status" "$replay_status" "$native_absence_status" "$compat_absence" "$final_exit" "$delete_key_digest" "$work_dir" <<'PY'
import hashlib,json,pathlib,sys
root,out,baseline,head,run,engine,ta,pa,pb,expected,project,old,new,scope,replacement_scope,delete_status,replay_status,native_absence,absence,final_exit,key_digest,work=sys.argv[1:]
contract_path=pathlib.Path(root)/"docs/compatibility/p13-5/p13-5a-convergence-contract.json"
contract=json.loads(contract_path.read_text())
def digest(path):
 h=hashlib.sha256()
 with open(path,'rb') as f:
  for c in iter(lambda:f.read(1048576),b''): h.update(c)
 return h.hexdigest()
refresh_raw=json.loads(pathlib.Path(work,"refresh.json").read_text()); normal=json.loads(pathlib.Path(work,"normal.json").read_text())
prior=json.loads(pathlib.Path(work,"state-before.json").read_text())
# OpenTofu's refresh-only show uses resource_changes, while the evidence
# contract names the observed remote drift explicitly. Preserve the complete
# raw plan and add only that derived, machine-checkable view.
refresh={'format_version':refresh_raw.get('format_version'),'planned_values':refresh_raw.get('planned_values',{}),'prior_state':{'format_version':'1.0','values':prior.get('values',{})},'resource_drift':refresh_raw.get('resource_drift',[])}
normal['prior_state']={'format_version':'1.0','values':prior.get('values',{})}
managed=lambda d:[x for x in d.get("resource_changes",[]) if x.get("address")=="openstack_networking_network_v2.managed"]
rc=managed(normal); actions=rc[0].get("change",{}).get("actions",[]) if len(rc)==1 else []
drift=refresh.get('resource_drift',[])
before_list=json.loads(pathlib.Path(work,'native-list-before.json').read_text()).get('items',[])
after_delete_list=json.loads(pathlib.Path(work,'native-list-after-delete.json').read_text()).get('items',[])
after_replay_list=json.loads(pathlib.Path(work,'native-list-after-replay.json').read_text()).get('items',[])
after_list=json.loads(pathlib.Path(work,'native-list-after.json').read_text()).get('items',[])
before_ids=[x.get('metadata',{}).get('id') for x in before_list]
after_delete_ids=[x.get('metadata',{}).get('id') for x in after_delete_list]
after_replay_ids=[x.get('metadata',{}).get('id') for x in after_replay_list]
after_ids=[x.get('metadata',{}).get('id') for x in after_list]
unrelated=[x for x in before_list if x.get('metadata',{}).get('id')!=old and x.get('metadata',{}).get('owner_scope')==scope]
normal_changes=normal.get('resource_changes',[])
unrelated_changes=[x for x in normal_changes if x.get('address')!='openstack_networking_network_v2.managed' and x.get('change',{}).get('actions') not in ([],['no-op'])]
if len(drift)!=1 or drift[0].get('address')!='openstack_networking_network_v2.managed' or drift[0].get('change',{}).get('actions')!=['delete']: raise SystemExit('refresh-only was not exactly the observed remote deletion')
if len(rc)!=1 or actions!=['create'] or unrelated_changes: raise SystemExit('normal plan was not exactly one recreation')
if old not in before_ids: raise SystemExit('native before observation missing old identity')
new_count=after_ids.count(new)
old_absent=old not in after_ids and native_absence=='404'
if new_count != 1 or not old_absent: raise SystemExit('native after observation does not prove exact replacement')
replay_same_terminal_state=after_delete_ids == after_replay_ids and old not in after_replay_ids
row={'resource':'openstack_networking_network_v2','scenario':'native-delete-drift','native_change':'remote absence','surface':'native_api','native_surface_status':'defined','operation':'deletion','terraform_address':'openstack_networking_network_v2.managed','canonical_id_before':old,'canonical_id_after_native_mutation':None,'canonical_id_after_reapply':new,'owner_scope':scope,'refresh_only_actions':[{'address':'openstack_networking_network_v2.managed','actions':refresh['resource_drift'][0].get('change',{}).get('actions',[]) if refresh['resource_drift'] else []}],'normal_plan_actions':[{'address':'openstack_networking_network_v2.managed','actions':actions,'replacement':actions==['create']}],'unrelated_changes_count':len(unrelated_changes),'old_resource_absent':old_absent,'new_resource_count':new_count,'final_plan_noop':int(final_exit)==0,'backend':'sqlite','head_sha':head,'provider_modified':False,'result':'passed','native_delete':{'http_path':f'/o3k/v1/network/networks/{old}','status':int(delete_status),'response_body':pathlib.Path(work,'delete.body').read_text() or None,'replay_status':int(replay_status),'replay_response_body':pathlib.Path(work,'replay.body').read_text() or None,'idempotency_key':'[REDACTED]','idempotency_key_sha256':key_digest,'problem_details':None,'operation_id':None,'replay_result':{'same_idempotency_key':True,'same_terminal_canonical_absence':replay_same_terminal_state,'second_destructive_effect_observed':not replay_same_terminal_state}},'native_absence_http_status':int(native_absence),'compatibility_absence_http_status':int(absence),'leak_or_foreign_state':{'old_absent':old_absent,'scope_unchanged':replacement_scope==scope,'unrelated_changes':len(unrelated_changes)==0,'same_scope_other_resources':len(unrelated)},'canonical_observations':{'before':{'ids':before_ids,'old_present':old in before_ids},'after_delete':{'ids':after_delete_ids,'old_present':old in after_delete_ids},'after_delete_replay':{'ids':after_replay_ids,'native_absence_http_status':int(native_absence),'old_present':old in after_replay_ids},'after_reapply':{'ids':after_ids,'replacement_count':new_count,'old_present':old in after_ids,'replacement_scope':replacement_scope}},'plan_observation':{'refresh-only':[refresh],'normal':[normal]}}
rows=[]
for item in contract['resources']:
 for attr in item.get('mutable_attributes',[]): rows.append({'resource':item['resource'],'scenario':'native-mutable-drift','native_change':attr,'reason':'native_surface_not_defined: no accepted native PUT/PATCH surface exists','native_surface_status':'native_surface_not_defined','result':'blocked'})
 rows.append(row if item['resource']==row['resource'] else {'resource':item['resource'],'scenario':'native-delete-drift','native_change':'remote absence','reason':'execution_profile_unavailable: no accepted real native execution boundary selected for this resource','native_surface_status':'defined' if item['resource'] in {'openstack_compute_instance_v2','openstack_blockstorage_volume_v3'} else 'native_surface_not_defined','result':'blocked'})
for r in rows: r.update({'surface':'native_api','operation':'mutable' if r['scenario']=='native-mutable-drift' else 'deletion','backend':'sqlite','head_sha':head,'provider_modified':False})
doc={'artifact_type':'o3k-p13-5c-native-drift-evidence','schema_version':1,'phase':'P13.5C','profile':'p13-iac-compatibility-v1','status':'blocked','canonical_authority':'o3k','canonical_authority_observation_route':'GET /o3k/v1/network/networks','provider_modified':False,'p13_5a_contract_sha256':hashlib.sha256(contract_path.read_bytes()).hexdigest(),'tested_o3k_head_sha':head,'baseline_manifest':str(pathlib.Path(baseline).resolve()),'toolchain':{'opentofu':'1.12.6','opentofu_version_output':engine,'opentofu_archive_sha256':digest(ta),'provider':'terraform-provider-openstack/openstack 3.4.0','provider_archive_sha256':digest(pa),'provider_binary_sha256':digest(pb),'provider_sha256_expected':expected,'provider_modified':False},'owner_scope':project,'evidence_work_dir':work,'scenarios':rows}
pathlib.Path(out).write_text(json.dumps(doc,indent=2,sort_keys=True)+'\n')
PY
python3 "$root_dir/scripts/validate_p13_5c_evidence.py" --allow-blocked "$output"
echo "P13.5C native Network DELETE evidence written: $output"
echo "P13.5C execution status: BLOCKED (Network passed; other executable surfaces not run)"
exit 2
