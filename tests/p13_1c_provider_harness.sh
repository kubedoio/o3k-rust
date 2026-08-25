#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
o3kd="${O3K_P13_O3KD:-${root_dir}/target/debug/o3kd}"
tofu="${O3K_P13_TOFU:-tofu}"
port="${O3K_P13_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()
PY
)}"
password="${O3K_P13_PASSWORD:-p13-1c-disposable-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?P13.1C requires O3K_P13_PROVIDER_ARCHIVE}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?P13.1C requires O3K_P13_TOFU_ARCHIVE}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?P13.1C requires O3K_P13_PROVIDER_BINARY}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?P13.1C requires O3K_P13_PROVIDER_SHA256}"
[[ -x "$o3kd" ]] || { echo "missing o3kd: $o3kd" >&2; exit 2; }
work_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p13-1c.XXXXXX")"
cleanup() { if [[ -n "${o3kd_pid:-}" ]]; then kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; fi; }
trap cleanup EXIT
trace_path="$work_dir/trace.jsonl"; data_dir="$work_dir/data"
: >"$trace_path"
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-1c-token-signing-key-012345678901234567890123" O3K_COMPATIBILITY_TRACE_PATH="$trace_path" \
  "$o3kd" --listen-addr "127.0.0.1:${port}" --data-dir "$data_dir" >"$work_dir/o3kd.log" 2>&1 &
o3kd_pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1 && break; sleep 0.1; done
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null
auth_headers="$work_dir/auth.headers"
curl -fsS -D "$auth_headers" -o "$work_dir/auth.json" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"${password}\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$auth_headers" | tr -d '\r')"
image_name="p13-1d-image"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v2/images" \
  --data "{\"name\":\"${image_name}\",\"visibility\":\"private\",\"container_format\":\"bare\",\"disk_format\":\"raw\"}" >"$work_dir/image.json"
image_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$work_dir/image.json")"
printf 'p13-1d-image\n' >"$work_dir/image-content"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/octet-stream' --data-binary "@$work_dir/image-content" \
  -X PUT "http://127.0.0.1:${port}/v2/images/${image_id}/file" >/dev/null
mkdir "$work_dir/cases"
cat >"$work_dir/provider.tf" <<EOF
terraform {
  required_providers {
    openstack = {
      source = "terraform-provider-openstack/openstack"
      version = "3.4.0"
    }
  }
}
provider "openstack" {
  auth_url = "http://127.0.0.1:${port}"
  user_name = "admin"
  password = var.password
  tenant_id = "${project_id}"
  max_retries = 0
}
variable "password" { type = string }
EOF
cat >"$work_dir/case-results.json" <<'EOF'
{}
EOF
network_id=""
run_case() {
  local name="$1" body="$2" case_dir="$work_dir/cases/$1"
  if [[ "$body" == *"__NETWORK_ID__"* ]]; then
    if [[ -z "$network_id" ]]; then
      echo "network fixture was not created before $name; using a deterministic discovery-only reference" >&2
      network_id="00000000-0000-0000-0000-000000000099"
    fi
    body="${body//__NETWORK_ID__/$network_id}"
  fi
  body="${body//__IMAGE_ID__/$image_id}"
  mkdir "$case_dir"; cp "$work_dir/provider.tf" "$case_dir/provider.tf"; printf '%s\n' "$body" >"$case_dir/main.tf"
  printf 'password = "%s"\n' "$password" >"$case_dir/terraform.tfvars"
  if ! (cd "$case_dir" && "$tofu" init -input=false -upgrade=false); then
    echo "P13.1C provider initialization failed for $name" >&2
    return 1
  fi
  set +e; (cd "$case_dir" && "$tofu" apply -input=false -auto-approve 2>&1) >"$case_dir/apply.log"; local rc=$?; set -e
  if [[ "$rc" != 0 ]]; then
    echo "P13.1C $name result: provider operation failed (recorded as discovery evidence)" >&2
    tail -18 "$case_dir/apply.log" >&2
  fi
  if [[ "$name" == "network" && "$rc" == 0 ]]; then
    network_id="$(cd "$case_dir" && "$tofu" show -json | python3 -c 'import json,sys; print(json.load(sys.stdin)["values"]["root_module"]["resources"][0]["values"]["id"])')"
  fi
  python3 - "$work_dir/case-results.json" "$name" "$rc" "$case_dir/apply.log" <<'PY'
import json, pathlib, sys
p, name, rc, log = sys.argv[1:]
doc = json.loads(pathlib.Path(p).read_text())
doc[name] = {"exit_code": int(rc), "output": pathlib.Path(log).read_text()[-12000:]}
pathlib.Path(p).write_text(json.dumps(doc, indent=2, sort_keys=True) + "\n")
PY
}
run_case keypair $'resource "openstack_compute_keypair_v2" "probe" {\n  name = "p13-1c-keypair"\n  public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA p13-1c"\n}'
run_case network $'resource "openstack_networking_network_v2" "probe" {\n  name = "p13-1c-network"\n}'
run_case subnet $'resource "openstack_networking_subnet_v2" "probe" {\n  network_id = "__NETWORK_ID__"\n  cidr = "192.0.2.0/24"\n  ip_version = 4\n}'
run_case port $'resource "openstack_networking_port_v2" "probe" {\n  network_id = "__NETWORK_ID__"\n  name = "p13-1c-port"\n}'
run_case server $'resource "openstack_compute_instance_v2" "probe" {\n  name = "p13-1c-server"\n  image_id = "__IMAGE_ID__"\n  flavor_id = "00000000-0000-0000-0000-000000000001"\n  network { uuid = "__NETWORK_ID__" }\n}'
export O3K_P13_TOFU="$tofu" O3K_P13_TOFU_ARCHIVE="$tofu_archive" O3K_P13_PROVIDER_ARCHIVE="$provider_archive" O3K_P13_PROVIDER_BINARY="$provider_binary" O3K_P13_P13C_RESULTS="$work_dir/case-results.json" O3K_P13_RAW_EVIDENCE="$trace_path" O3K_P13_EVIDENCE_OUTPUT="${O3K_P13_EVIDENCE_OUTPUT:-$root_dir/target/p13-1c/provider-contract.json}"
python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
python3 "$root_dir/scripts/p13_provider_contract.py" --assemble-managed
python3 "$root_dir/scripts/p13_provider_contract.py" --validate-artifact "$O3K_P13_EVIDENCE_OUTPUT"
echo "P13.1C managed-resource discovery completed; see $O3K_P13_EVIDENCE_OUTPUT"
