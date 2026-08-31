#!/usr/bin/env bash
set -euo pipefail

# P13.5B portable real-provider cases.  This deliberately covers only the
# resource cases that can be exercised without a privileged backend in the
# current profile; unavailable cases are emitted as controlled BLOCKED rows.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?O3K_P13_PROVIDER_ARCHIVE is required}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?O3K_P13_TOFU_ARCHIVE is required}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?O3K_P13_PROVIDER_SHA256 is required}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
output="${O3K_P13_5B_EVIDENCE_OUTPUT:-$root_dir/target/p13-5b/refresh-import-evidence.json}"
baseline_result="${P13_5B_BASELINE_RESULT:-blocked}"
password="${O3K_P13_PASSWORD:-p13-5b-refresh-import-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
work_dir="$(mktemp -d /var/tmp/o3k-p13-5b.XXXXXX)"
pid=""

cleanup() {
  if [[ -n "$pid" ]]; then
    kill "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true
  fi
  rm -rf "$work_dir"
}
trap cleanup EXIT

if [[ "$baseline_result" == verified && "${P13_5A_RUN_BASELINE:-0}" != 1 ]]; then
  echo "P13.5B BLOCKED: verified baseline must come from the parent harness baseline execution" >&2
  exit 2
fi
if [[ "$baseline_result" != verified && "${P13_5B_EXPLORATORY:-0}" != 1 ]]; then
  echo "P13.5B BLOCKED: existing P13.2-P13.4 baseline is not verified; use the parent harness or explicitly label an exploratory run" >&2
  exit 2
fi

[[ -x "$tofu" ]] || { echo "P13.5B BLOCKED: OpenTofu is not executable: $tofu" >&2; exit 2; }
[[ -x "$o3kd" ]] || { echo "P13.5B BLOCKED: o3kd is not executable: $o3kd" >&2; exit 2; }
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools

mkdir -p "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
cp "$provider_binary" "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
chmod 0755 "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation {
  filesystem_mirror {
    path = "$work_dir/mirror"
    include = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
  direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF

O3K_BOOTSTRAP_PASSWORD="$password" \
O3K_TOKEN_SIGNING_KEY="p13-5b-token-signing-key-012345678901234567890123" \
O3K_COMPATIBILITY_TRACE_PATH="$work_dir/trace.jsonl" \
  "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do
  curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done
curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' \
  -X POST "http://127.0.0.1:$port/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
[[ -n "$token" ]] || { echo "P13.5B BLOCKED: authentication did not return a token" >&2; exit 2; }

export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
project_dir="$work_dir/project"
mkdir -p "$project_dir"
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
EOF
cd "$project_dir"
"$tofu" init -input=false -upgrade=false >/dev/null

plan() {
  local label="$1"
  "$tofu" plan -input=false -refresh-only -out="$work_dir/$label-refresh.tfplan" >/dev/null
  "$tofu" show -json "$work_dir/$label-refresh.tfplan" >"$work_dir/$label-refresh.json"
  "$tofu" plan -input=false -out="$work_dir/$label-normal.tfplan" >/dev/null
  "$tofu" show -json "$work_dir/$label-normal.tfplan" >"$work_dir/$label-normal.json"
}

cat >keypair.tf <<'EOF'
resource "openstack_compute_keypair_v2" "managed" {
  name = "p13-5b-keypair"
  public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
}
EOF
"$tofu" apply -input=false -auto-approve >/dev/null
plan keypair-read-1
plan keypair-read-2
keypair_id="p13-5b-keypair"
keypair_stable_count="$(curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs" | python3 -c 'import json,sys; print(sum(1 for x in json.load(sys.stdin)["keypairs"] if x["keypair"]["name"] == "p13-5b-keypair"))')"
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs/p13-5b-keypair" >"$work_dir/keypair-stable-projection.json"
"$tofu" destroy -input=false -auto-approve >/dev/null
keypair_stable_cleanup="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs/p13-5b-keypair")"

curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST \
  "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs" \
  --data '{"keypair":{"name":"p13-5b-import-keypair","public_key":"ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}}' >/dev/null
cat >keypair.tf <<'EOF'
resource "openstack_compute_keypair_v2" "imported" {
  name = "p13-5b-import-keypair"
  public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
}
EOF
keypair_trace_start="$(wc -l <"$work_dir/trace.jsonl")"
"$tofu" import -input=false openstack_compute_keypair_v2.imported p13-5b-import-keypair >/dev/null
"$tofu" plan -input=false -out="$work_dir/keypair-import-normal.tfplan" >/dev/null
"$tofu" show -json "$work_dir/keypair-import-normal.tfplan" >"$work_dir/keypair-import-normal.json"
keypair_count="$(curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs" | python3 -c 'import json,sys; print(sum(1 for x in json.load(sys.stdin)["keypairs"] if x["keypair"]["name"] == "p13-5b-import-keypair"))')"
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs/p13-5b-import-keypair" >"$work_dir/keypair-import-projection.json"
"$tofu" destroy -input=false -auto-approve >/dev/null
keypair_import_cleanup="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.1/$project_id/os-keypairs/p13-5b-import-keypair")"
rm -f keypair.tf

cat >network.tf <<'EOF'
resource "openstack_networking_network_v2" "managed" {
  name = "p13-5b-network"
  admin_state_up = true
}
EOF
"$tofu" apply -input=false -auto-approve >/dev/null
network_id="$("$tofu" show -json | python3 -c 'import json,sys; print(next(x["values"]["id"] for x in json.load(sys.stdin)["values"]["root_module"]["resources"] if x["address"]=="openstack_networking_network_v2.managed"))')"
plan network-read-1
plan network-read-2
network_stable_count="$(curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks" | python3 -c 'import json,sys; print(sum(1 for x in json.load(sys.stdin)["networks"] if x["name"] == "p13-5b-network"))')"
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$network_id" >"$work_dir/network-stable-projection.json"
"$tofu" destroy -input=false -auto-approve >/dev/null
network_stable_cleanup="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$network_id")"

network_response="$work_dir/import-network.json"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST \
  "http://127.0.0.1:$port/v2.0/networks" --data '{"network":{"name":"p13-5b-import-network"}}' >"$network_response"
import_network_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["network"]["id"])' "$network_response")"
cat >network.tf <<EOF
resource "openstack_networking_network_v2" "imported" { name = "p13-5b-import-network" }
EOF
network_trace_start="$(wc -l <"$work_dir/trace.jsonl")"
"$tofu" import -input=false openstack_networking_network_v2.imported "$import_network_id" >/dev/null
"$tofu" plan -input=false -out="$work_dir/network-import-normal.tfplan" >/dev/null
"$tofu" show -json "$work_dir/network-import-normal.tfplan" >"$work_dir/network-import-normal.json"
network_count="$(curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks" | python3 -c 'import json,sys; wanted=sys.argv[1]; print(sum(1 for x in json.load(sys.stdin)["networks"] if x["id"] == wanted))' "$import_network_id")"
network_projection="$(curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$import_network_id")"
printf '%s\n' "$network_projection" >"$work_dir/network-import-projection.json"
"$tofu" destroy -input=false -auto-approve >/dev/null
network_import_cleanup="$(curl -sS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v2.0/networks/$import_network_id")"

python3 - "$root_dir" "$output" "$work_dir" "$tofu" "$tofu_archive" "$provider_archive" "$provider_binary" "$provider_sha" "$project_id" "$network_id" "$import_network_id" "$keypair_stable_count" "$keypair_count" "$network_stable_count" "$network_count" "$keypair_stable_cleanup" "$keypair_import_cleanup" "$network_stable_cleanup" "$network_import_cleanup" "$keypair_trace_start" "$network_trace_start" "$baseline_result" <<'PY'
import hashlib
import json
import pathlib
import sys

root, output, work, tofu, tofu_archive, provider_archive, provider_binary, provider_sha, project, network_id, import_network_id, keypair_stable_count, keypair_count, network_stable_count, network_count, keypair_stable_cleanup, keypair_import_cleanup, network_stable_cleanup, network_import_cleanup, keypair_trace_start, network_trace_start, baseline_result = sys.argv[1:]
work = pathlib.Path(work)

def digest(path):
    h = hashlib.sha256()
    with open(path, "rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()

def actions(path):
    plan = json.loads((work / path).read_text())
    return [action for change in plan.get("resource_changes", []) for action in change["change"]["actions"]]

def cleanup_result(status):
    return "passed" if status == "404" else "blocked"

def provider_read_routes(start, resource, identity):
    expected = {
        "openstack_compute_keypair_v2": "/os-keypairs/",
        "openstack_networking_network_v2": "/networks/",
    }[resource]
    routes = []
    records = (work / "trace.jsonl").read_text().splitlines()
    for ordinal, line in enumerate(records):
        if ordinal < int(start):
            continue
        record = json.loads(line)
        headers = record.get("request_headers", {})
        user_agent = headers.get("user-agent", "")
        method = record.get("method") or record.get("request_method")
        path = record.get("path") or record.get("request_path")
        if "Terraform Provider OpenStack/3.4.0" in user_agent and method == "GET" and path and expected in path and identity in path:
            routes.append({"method": method, "path": path, "ordinal": ordinal})
    return routes

def projection(path, resource):
    document = json.loads((work / path).read_text())
    if resource == "openstack_compute_keypair_v2":
        item = document["keypair"]
        return {"id": item["name"], "owner_scope": item.get("project_id", item.get("tenant_id", project))}
    item = document["network"]
    return {"id": item["id"], "owner_scope": item.get("project_id", item.get("tenant_id"))}

def scenario(resource, kind, canonical, import_id, refresh_files, normal_files, cleanup, duplicate_count=0, result="passed", reason=None, trace_start=0, projection_file=None):
    normal = actions(normal_files[-1]) if normal_files else []
    trace_routes = provider_read_routes(trace_start, resource, canonical)
    observed = projection(projection_file, resource) if projection_file else None
    minimum_routes = 2 if kind == "stable-read" else 1
    if result == "passed" and len(trace_routes) < minimum_routes:
        result = "blocked"
        reason = f"structured compatibility trace has {len(trace_routes)} provider reads; expected at least {minimum_routes}"
    if result == "passed" and int(duplicate_count) != 1:
        result = "blocked"
        reason = f"canonical compatibility projection count was {duplicate_count}, expected exactly one"
    if result == "passed" and cleanup != "passed":
        result = "blocked"
        reason = "canonical compatibility projection did not return 404 after cleanup"
    if result == "passed" and observed != {"id": canonical, "owner_scope": project}:
        result = "blocked"
        reason = f"canonical projection identity mismatch: {observed}"
    if result == "passed" and kind == "import" and not import_id:
        result = "blocked"
        reason = "provider import identifier was empty"
    item = {
        "resource": resource,
        "scenario": kind,
        "canonical_id": canonical,
        "owner_scope": project,
        "provider_import_id": import_id,
        "provider_state_id": canonical if result == "passed" else None,
        "first_read_route": trace_routes[0]["method"] + " " + trace_routes[0]["path"] if trace_routes else "",
        "trace_observation": {"provider_read_routes": trace_routes, "trace_start_ordinal": int(trace_start)},
        "canonical_identity_observation": {
            "source": "compatibility_projection_read",
            "owner_scope": project,
            "resource_id": observed["id"] if observed else None,
            "observed_owner_scope": observed["owner_scope"] if observed else None,
        },
        "plan_actions": normal,
        "refresh_plan_actions": [actions(name) for name in refresh_files],
        "normal_plan_actions": [actions(name) for name in normal_files],
        "final_plan_noop": result == "passed" and normal in ([], ["no-op"]),
        "canonical_duplicate_count": max(int(duplicate_count) - 1, 0) if result == "passed" else None,
        "canonical_resource_count": int(duplicate_count) if result == "passed" else None,
        "cleanup_result": cleanup,
        "backend": "sqlite",
        "head_sha": __import__("subprocess").check_output(["git", "-C", root, "rev-parse", "HEAD"], text=True).strip(),
        "result": result,
    }
    if reason:
        item["reason"] = reason
    return item

scenarios = [
    scenario("openstack_compute_keypair_v2", "stable-read", "p13-5b-keypair", "", ["keypair-read-1-refresh.json", "keypair-read-2-refresh.json"], ["keypair-read-1-normal.json", "keypair-read-2-normal.json"], cleanup_result(keypair_stable_cleanup), keypair_stable_count, trace_start=0, projection_file="keypair-stable-projection.json"),
    scenario("openstack_compute_keypair_v2", "import", "p13-5b-import-keypair", "p13-5b-import-keypair", [], ["keypair-import-normal.json"], cleanup_result(keypair_import_cleanup), keypair_count, trace_start=keypair_trace_start, projection_file="keypair-import-projection.json"),
    scenario("openstack_networking_network_v2", "stable-read", network_id, "", ["network-read-1-refresh.json", "network-read-2-refresh.json"], ["network-read-1-normal.json", "network-read-2-normal.json"], cleanup_result(network_stable_cleanup), network_stable_count, trace_start=0, projection_file="network-stable-projection.json"),
    scenario("openstack_networking_network_v2", "import", import_network_id, import_network_id, [], ["network-import-normal.json"], cleanup_result(network_import_cleanup), network_count, trace_start=network_trace_start, projection_file="network-import-projection.json"),
]
unrun = {
    "openstack_networking_subnet_v2": "requires relationship fixture and is not part of this portable core runner",
    "openstack_networking_port_v2": "provider 3.4.0 supports passthrough import, but does not reconstruct configurable fixed_ip/security_group_ids; the bounded configuration-specific import case is blocked by a non-no-op plan",
    "openstack_compute_instance_v2": "requires image/compute fixture and attachment inspection",
    "openstack_networking_secgroup_v2": "requires policy fixture and default-rule observation",
    "openstack_networking_secgroup_rule_v2": "requires policy-parent fixture",
    "openstack_networking_router_v2": "existing router gate requires rerun with its password environment set",
    "openstack_networking_router_interface_v2": "relationship fixture and parent-retention proof not yet available",
    "openstack_networking_floatingip_v2": "requires public-address pool/binding fixture",
    "openstack_blockstorage_volume_v3": "native volume service unavailable without configured storage backend",
    "openstack_compute_volume_attach_v2": "requires disposable LVM backend and parent fixture",
}
for resource, reason in unrun.items():
    for kind in ("stable-read", "import"):
        scenarios.append({
            "resource": resource, "scenario": kind, "canonical_id": "", "owner_scope": project,
            "provider_import_id": "", "provider_state_id": None, "first_read_route": "", "plan_actions": [],
            "refresh_plan_actions": [], "final_plan_noop": False, "canonical_duplicate_count": None,
            "canonical_resource_count": None,
            "normal_plan_actions": [],
            "trace_observation": {"provider_read_routes": [], "trace_start_ordinal": None},
            "canonical_identity_observation": {"source": "not_run", "owner_scope": project, "resource_id": None},
            "cleanup_result": "not_run", "backend": "sqlite", "head_sha": scenarios[0]["head_sha"],
            "result": "blocked",
            "reason": reason,
        })
document = {
    "artifact_type": "o3k-p13-5b-refresh-import-evidence",
    "schema_version": 1,
    "phase": "P13.5B",
    "profile": "p13-iac-compatibility-v1",
    "status": "passed" if all(s["result"] == "passed" for s in scenarios) else "blocked",
    "execution_mode": "gated" if baseline_result == "verified" else "exploratory_blocked_baseline",
    "tested_o3k_head_sha": scenarios[0]["head_sha"],
    "starting_main_sha": __import__("subprocess").check_output(["git", "-C", root, "merge-base", "HEAD", "origin/main"], text=True).strip(),
    "existing_p13_baseline": {
        "status": baseline_result,
        "classification": "environment_and_existing_gate_limitations",
        "required_gates": [
            "tests/p13_2_core_lifecycle.sh", "tests/p13_2b_subnet_lifecycle.sh",
            "tests/p13_2c_port_lifecycle.sh", "tests/p13_2d_server_lifecycle.sh",
            "tests/p13_3_security_group_provider.sh", "tests/p13_3_security_group_port_provider.sh",
            "tests/p13_3_router_provider.sh", "tests/p13_3_floating_ip_provider.sh",
            "tests/p13_4_provider_volume_smoke.sh", "tests/p13_4_provider_volume_attachment_smoke.sh",
            "tests/p13_4_storage_lifecycle.sh",
        ],
        "completed_before_block": [
            "tests/p13_2_core_lifecycle.sh", "tests/p13_2b_subnet_lifecycle.sh",
            "tests/p13_2c_port_lifecycle.sh", "tests/p13_2d_server_lifecycle.sh",
            "tests/p13_3_security_group_provider.sh", "tests/p13_3_security_group_port_provider.sh",
            "tests/p13_3_router_provider.sh", "tests/p13_3_floating_ip_provider.sh",
        ],
        "failed_gate": "tests/p13_4_provider_volume_smoke.sh",
        "failure": "native volume service unavailable",
        "provider_import_limitation": "port fixed_ip/security_group_ids are computed all_* observations in upstream 3.4.0 and are not reconstructed as configurable state; the baseline uses the supported identity/name/network import subset",
        "backend_limitations": ["native volume service unavailable", "VolumeAttachment requires disposable LVM"],
    },
    "canonical_authority": "o3k",
    "manual_state_edits": False,
    "toolchain": {
        "opentofu": "1.12.6", "opentofu_archive_sha256": digest(tofu_archive),
        "provider": "terraform-provider-openstack/openstack 3.4.0",
        "provider_archive_sha256": digest(provider_archive),
        "provider_binary_sha256": digest(provider_binary), "provider_sha256": provider_sha,
        "provider_modified": False,
    },
    "scenarios": scenarios,
}
pathlib.Path(output).parent.mkdir(parents=True, exist_ok=True)
pathlib.Path(output).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
PY
python3 "$root_dir/scripts/validate_p13_5b_evidence.py" "$output"
echo "P13.5B evidence written: $output"
[[ "$(jq -r .status "$output")" == passed ]] || { echo "P13.5B run blocked: inspect $output" >&2; exit 2; }
