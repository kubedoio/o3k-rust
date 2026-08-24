#!/usr/bin/env bash
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
o3kd="${O3K_P13_O3KD:-${root_dir}/target/debug/o3kd}"
tofu="${O3K_P13_TOFU:-tofu}"
port="${O3K_P13_PORT:-$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)}"
password="${O3K_P13_PASSWORD:-p13-1b-disposable-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
image_name="${O3K_P13_IMAGE_NAME:-p13-1b-image}"
flavor_name="${O3K_P13_FLAVOR_NAME:-test.small}"

for required in O3K_P13_TOFU_ARCHIVE O3K_P13_PROVIDER_ARCHIVE O3K_P13_PROVIDER_BINARY O3K_P13_PROVIDER_SHA256; do
  if [[ -z "${!required:-}" ]]; then
    echo "P13.1B requires ${required} for upstream tool verification" >&2
    exit 2
  fi
done
if [[ ! -x "$o3kd" ]]; then
  echo "P13.1B requires a built o3kd at $o3kd; run cargo build -p o3kd" >&2
  exit 2
fi

work_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-p13-1b.XXXXXX")"
cleanup() {
  if [[ -n "${o3kd_pid:-}" ]]; then
    kill "$o3kd_pid" 2>/dev/null || true
    wait "$o3kd_pid" 2>/dev/null || true
  fi
  rm -rf "$work_dir"
}
trap cleanup EXIT

trace_path="$work_dir/trace.jsonl"
data_dir="$work_dir/data"
log_path="$work_dir/o3kd.log"
O3K_BOOTSTRAP_PASSWORD="$password" \
O3K_TOKEN_SIGNING_KEY="p13-1b-token-signing-key-012345678901234567890123" \
O3K_COMPATIBILITY_TRACE_PATH="$trace_path" \
  "$o3kd" --listen-addr "127.0.0.1:${port}" --data-dir "$data_dir" >"$log_path" 2>&1 &
o3kd_pid=$!
for _ in $(seq 1 120); do
  if curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null 2>&1; then break; fi
  sleep 0.1
done
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null

auth_headers="$work_dir/auth.headers"
curl -fsS -D "$auth_headers" -o "$work_dir/auth.json" \
  -H 'Content-Type: application/json' -X POST "http://127.0.0.1:${port}/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"${password}\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$auth_headers" | tr -d '\r')"
if [[ -z "$token" ]]; then echo "P13.1B did not receive an authentication token" >&2; exit 1; fi
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' \
  -X POST "http://127.0.0.1:${port}/v2/images" \
  --data "{\"name\":\"${image_name}\",\"visibility\":\"private\",\"container_format\":\"bare\",\"disk_format\":\"raw\"}" \
  >"$work_dir/image.json"
image_id="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["id"])' "$work_dir/image.json")"
printf 'p13-1b-image-fixture\n' >"$work_dir/image-content"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/octet-stream' \
  --data-binary "@$work_dir/image-content" \
  -X PUT "http://127.0.0.1:${port}/v2/images/${image_id}/file" >/dev/null

project="$work_dir/project"
mkdir "$project"
cp "$root_dir/tests/p13_1/real-project"/*.tf "$project/"
cat >"$project/terraform.tfvars" <<EOF
auth_url = "http://127.0.0.1:${port}"
user_name = "admin"
password = "${password}"
project_id = "${project_id}"
image_name = "${image_name}"
flavor_name = "${flavor_name}"
EOF

export O3K_P13_TOFU="$tofu"
export O3K_P13_TOFU_PROJECT="$project"
export O3K_P13_RAW_EVIDENCE="$trace_path"
export O3K_P13_EVIDENCE_OUTPUT="${O3K_P13_EVIDENCE_OUTPUT:-${root_dir}/target/p13-1b/provider-contract.json}"
export O3K_P13_EXPECTED_FAILURE="${O3K_P13_EXPECTED_FAILURE:-1}"
export O3K_P13_PHASE=p13.1b
export TF_VAR_auth_url="http://127.0.0.1:${port}"
export TF_VAR_user_name=admin
export TF_VAR_password="$password"
export TF_VAR_project_id="$project_id"
export TF_VAR_image_name="$image_name"
export TF_VAR_flavor_name="$flavor_name"

python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools
python3 "$root_dir/scripts/p13_provider_contract.py" --run-real
python3 "$root_dir/scripts/p13_provider_contract.py" --validate-artifact "$O3K_P13_EVIDENCE_OUTPUT"
echo "P13.1B real-provider harness completed; inspect findings in $O3K_P13_EVIDENCE_OUTPUT"
