#!/usr/bin/env bash
# P13.6 — Multi-project security and failure evidence
#
# Fail-closed dispatcher for slices B–F, visualised with the contracted
# equality artifacts, validators and security/failure matrix from P13.6A.
#
# Slices:
#   P13.6B — positive multi-project isolation
#   P13.6C — cross-project negative/security evidence
#   P13.6D — restart and durable recovery matrix
#   P13.6E — lost-response and client ambiguity evidence
#   P13.6F — aggregate closure
#
# This file: skeleton harness with shared helpers.
# Slice B–E scenarios are implemented in their respective PRs.

set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
contract="$root_dir/docs/compatibility/p13-6/p13-6a-security-failure-contract.json"

# ---------------------------------------------------------------------------
# Environment
# ---------------------------------------------------------------------------
tofu="${O3K_P13_TOFU:-}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:-}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:-}"
provider_binary="${O3K_P13_PROVIDER_BINARY:-}"
provider_sha="${O3K_P13_PROVIDER_SHA256:-}"

o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
evidence_dir="${O3K_P13_EVIDENCE_DIR:-$root_dir/target/p13-6}"
password="${O3K_P13_PASSWORD:-p13-6-password}"

# Extra tenant B
tenb_project="${O3K_EXTRA_TENANT_PROJECT_ID:-9f3c2b6e-5f2d-4b3a-9c8e-1a2b3c4d5e6f}"
tenb_name="${O3K_EXTRA_TENANT_PROJECT_NAME:-tenant-b}"
tenb_user="${O3K_EXTRA_TENANT_USER_ID:-6b0f5a2e-8c4d-4a7e-9b1f-2d3e4f5a6b7c}"
tenb_username="${O3K_EXTRA_TENANT_USER_NAME:-tenant-b-user}"
tenb_pass="${O3K_EXTRA_TENANT_PASSWORD:-tenant-b-password}"

# Project A (bootstrap admin)
proja_id="eba29e2d-53de-461d-ae91-ede7402713cb"
proja_name="admin"
proja_user="admin"

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
redact() {
    python3 -c "
import sys, json
def _redact(v):
    if isinstance(v, str):
        if any(p in v.lower() for p in ['password','x-auth-token','authorization','bearer']):
            return '<REDACTED>'
        return v
    return v
d=json.load(sys.stdin)
sys.stdout.write(json.dumps(d, default=str))
" < /dev/stdin 2>/dev/null || cat
}

start_o3kd() {
    local state_dir="$1"
    local o3kd_port="$2"
    local db_backend_arg=""
    if [[ -n "${O3K_DATABASE_BACKEND:-}" ]]; then
        db_backend_arg="--database-backend $O3K_DATABASE_BACKEND"
    fi
    local db_url_arg=""
    if [[ -n "${O3K_DATABASE_URL:-}" ]]; then
        db_url_arg="--database-url $O3K_DATABASE_URL"
    fi

    mkdir -p "$state_dir"
    # Clean any prior o3kd on this port
    kill_port "$o3kd_port" 2>/dev/null || true

    O3K_BOOTSTRAP_PASSWORD="$password" \
    O3K_TOKEN_SIGNING_KEY="p13-6-signing-key-at-least-32-bytes-long" \
    O3K_EXTRA_TENANT_PROJECT_ID="$tenb_project" \
    O3K_EXTRA_TENANT_PROJECT_NAME="$tenb_name" \
    O3K_EXTRA_TENANT_USER_ID="$tenb_user" \
    O3K_EXTRA_TENANT_USER_NAME="$tenb_username" \
    O3K_EXTRA_TENANT_PASSWORD="$tenb_pass" \
    "$o3kd" \
        --listen-addr "127.0.0.1:$o3kd_port" \
        --data-dir "$state_dir" \
        $db_backend_arg \
        $db_url_arg \
        &

    local o3kd_pid=$!
    echo "$o3kd_pid" > "$state_dir/o3kd.pid"

    # Wait for ready
    for attempt in $(seq 1 30); do
        if curl -sf "http://127.0.0.1:$o3kd_port/readyz" >/dev/null 2>&1; then
            break
        fi
        sleep 0.5
    done
    if ! curl -sf "http://127.0.0.1:$o3kd_port/readyz" >/dev/null 2>&1; then
        echo "o3kd failed to start on port $o3kd_port" >&2
        return 1
    fi
    echo "$o3kd_port"
}

stop_o3kd() {
    local state_dir="$1"
    local pid_file="$state_dir/o3kd.pid"
    if [[ -f "$pid_file" ]]; then
        kill "$(cat "$pid_file")" 2>/dev/null || true
        rm -f "$pid_file"
    fi
}

restart_daemon() {
    local state_dir="$1"
    local o3kd_port="$2"
    stop_o3kd "$state_dir"
    sleep 1
    start_o3kd "$state_dir" "$o3kd_port"
}

kill_port() {
    local port="$1"
    fuser -k "${port}/tcp" 2>/dev/null || true
}

get_token() {
    local auth_url="$1"
    local user="$2"
    local pass="$3"
    local project_name="$4"
    local token

    token=$(curl -sf -X POST "$auth_url/v3/auth/tokens" \
        -H "Content-Type: application/json" \
        -d "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"$user\",\"password\":\"$pass\"}}},\"scope\":{\"project\":{\"name\":\"$project_name\"}}}}" \
        -D - 2>/dev/null | grep -i "x-subject-token:" | awk '{print $2}' | tr -d '\r')
    echo "$token"
}

setup_tofu_workdir() {
    local work_dir="$1"
    local auth_url="$2"
    local tenant_id="$3"
    local proj_name="$4"

    mkdir -p "$work_dir"

    # Filesystem mirror for offline provider installation
    local mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
    mkdir -p "$mirror_dir"
    cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"

    # .terraformrc
    cat > "$work_dir/tofu.tfrc" <<'TFRC'
provider_installation {
  filesystem_mirror {
    path = "REPLACE_MIRROR"
  }
  direct {
    exclude = ["terraform-provider-openstack/openstack"]
  }
}
TFRC
    sed -i "s|REPLACE_MIRROR|${work_dir}/mirror|g" "$work_dir/tofu.tfrc"

    # provider.tf — will be customised per run; write a template
    cat > "$work_dir/provider.tf" <<PROV
terraform {
  required_version = "= 1.12.6"
  required_providers {
    openstack = {
      source  = "terraform-provider-openstack/openstack"
      version = "= 3.4.0"
    }
  }
}
provider "openstack" {
  auth_url      = "$auth_url"
  user_name     = "$proj_name"
  password      = "$password"
  tenant_id     = "$tenant_id"
  max_retries   = 0
}
PROV

    cd "$work_dir"
    TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" \
    TF_IN_AUTOMATION=1 \
    "$tofu" init -input=false -upgrade=false -no-color 2>&1 | tail -3
}

emit_scenario_row() {
    local phase="$1"
    local scenario="$2"
    local result="$3"
    shift 3
    local extra_json="$*"

    local head_sha
    head_sha="$(git -C "$root_dir" rev-parse HEAD 2>/dev/null || echo "unknown")"

    python3 -c "
import json, sys
row = {
    'phase': '$phase',
    'scenario': '$scenario',
    'tested_runtime_head_sha': '$head_sha',
    'backend': '${O3K_DATABASE_BACKEND:-sqlite}',
    'project_a_principal': 'admin',
    'project_a_project': '$proja_id',
    'project_b_principal': '$tenb_username',
    'project_b_project': '$tenb_project',
    'result': '$result',
    $(echo "$extra_json" | sed "s/'/\\\\'/g")
}
sys.stdout.write(json.dumps(row, indent=2) + '\n')
"
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
preflight() {
    # Validate contract exists
    if [[ ! -f "$contract" ]]; then
        echo "P13.6 BLOCKED: contract $contract not found" >&2
        exit 2
    fi

    # Validate contract structure
    python3 "$root_dir/scripts/validate_p13_6a_contract.py" || {
        echo "P13.6 BLOCKED: contract validation failed" >&2
        exit 2
    }

    # Toolchain verification
    if [[ -z "$tofu" || -z "$provider_binary" || -z "$provider_sha" ]]; then
        echo "P13.6 BLOCKED: set O3K_P13_TOFU, O3K_P13_PROVIDER_BINARY, O3K_P13_PROVIDER_SHA256" >&2
        exit 2
    fi
    if ! version="$("$tofu" version | head -n 1)"; then
        echo "P13.6 BLOCKED: OpenTofu executable could not be run" >&2
        exit 2
    fi
    [[ "$version" == *"OpenTofu v1.12.6"* ]] || {
        echo "P13.6 BLOCKED: wrong OpenTofu: $version" >&2
        exit 2
    }
    if ! python3 "$root_dir/scripts/p13_provider_contract.py" --verify-tools; then
        echo "P13.6 BLOCKED: tool provenance verification failed" >&2
        exit 2
    fi

    # o3kd binary
    if [[ ! -x "$o3kd" ]]; then
        echo "P13.6 BLOCKED: o3kd binary not found at $o3kd" >&2
        exit 2
    fi

    mkdir -p "$evidence_dir"
    echo "P13.6 preflight: PASS"
}

# ---------------------------------------------------------------------------
# Self-test
# ---------------------------------------------------------------------------
self_test() {
    echo "P13.6 self-test: starting"

    # 1. Validate contract
    python3 "$root_dir/scripts/validate_p13_6a_contract.py"
    echo "P13.6 self-test: contract validation: PASS"

    # 2. Validate evidence schema (self-test fixture)
    python3 "$root_dir/scripts/validate_p13_6_evidence.py" --self-test
    echo "P13.6 self-test: evidence schema: PASS"

    # 3. Fault proxy self-test (reuse P13.5E)
    python3 "$root_dir/scripts/p13_5e_fault_proxy.py" --self-test
    echo "P13.6 self-test: fault proxy: PASS"

    # 4. Smoke: two-project o3kd boot, token acquisition, init
    echo "P13.6 self-test: identity model smoke check"
    local state_dir
    state_dir=$(mktemp -d /tmp/p13-6-smoke-XXXXXX)
    local o3kd_port=19090
    start_o3kd "$state_dir" "$o3kd_port"

    # Get token A
    local auth_url="http://127.0.0.1:$o3kd_port"
    local token_a
    token_a=$(get_token "$auth_url" "$proja_user" "$password" "$proja_name")
    if [[ -z "$token_a" ]]; then
        echo "P13.6 self-test: FAILED to get token A" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: token A acquired: OK"

    # Get token B
    local token_b
    token_b=$(get_token "$auth_url" "$tenb_username" "$tenb_pass" "$tenb_name")
    if [[ -z "$token_b" ]]; then
        echo "P13.6 self-test: FAILED to get token B" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: token B acquired: OK"

    # Verify tokens are distinct
    if [[ "$token_a" == "$token_b" ]]; then
        echo "P13.6 self-test: FAILED — tokens A and B are identical" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: tokens A and B are distinct: OK"

    # Smoke: project A creates a network
    local net_resp
    net_resp=$(curl -sf -X POST "$auth_url/v2.0/networks" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_a" \
        -d '{"network":{"name":"smoke-net"}}')
    local net_id
    net_id=$(echo "$net_resp" | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])")
    if [[ -z "$net_id" ]]; then
        echo "P13.6 self-test: FAILED — project A network create failed" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: project A created network $net_id: OK"

    # Verify project B cannot see it (404)
    local b_status
    b_status=$(curl -s -o /dev/null -w "%{http_code}" \
        "$auth_url/v2.0/networks/$net_id" \
        -H "X-Auth-Token: $token_b")
    if [[ "$b_status" != "404" ]]; then
        echo "P13.6 self-test: FAILED — project B sees project A network (status $b_status)" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: project B cannot access A's network: PASS (404)"

    # Verify project A lists only its own
    local a_list
    a_list=$(curl -sf "$auth_url/v2.0/networks" -H "X-Auth-Token: $token_a" \
        | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('networks',[])))")
    if [[ "$a_list" != "1" ]]; then
        echo "P13.6 self-test: FAILED — expected 1 network for A, got $a_list" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: project A lists 1 network: OK"

    local b_list
    b_list=$(curl -sf "$auth_url/v2.0/networks" -H "X-Auth-Token: $token_b" \
        | python3 -c "import json,sys; d=json.load(sys.stdin); print(len(d.get('networks',[])))")
    if [[ "$b_list" != "0" ]]; then
        echo "P13.6 self-test: FAILED — expected 0 networks for B, got $b_list" >&2
        stop_o3kd "$state_dir"
        exit 2
    fi
    echo "P13.6 self-test: project B lists 0 networks: OK"

    # Cleanup
    stop_o3kd "$state_dir"
    rm -rf "$state_dir"
    echo "P13.6 self-test: ALL PASS"
    exit 0
}

# ---------------------------------------------------------------------------
# Dispatch
# ---------------------------------------------------------------------------
main() {
    if [[ "${P13_6_SELF_TEST:-0}" == 1 ]]; then
        self_test
    fi

    preflight

    if [[ "${P13_6B_RUN:-0}" == 1 ]]; then
        echo "P13.6B: not yet implemented"
        emit_scenario_row "P13.6B" "positive_multiproject_isolation" "blocked" '"details": {"reason": "slice_not_started"}'
        exit 2
    fi

    if [[ "${P13_6C_RUN:-0}" == 1 ]]; then
        echo "P13.6C: not yet implemented"
        emit_scenario_row "P13.6C" "cross_project_negative" "blocked" '"details": {"reason": "slice_not_started"}'
        exit 2
    fi

    if [[ "${P13_6D_RUN:-0}" == 1 ]]; then
        echo "P13.6D: not yet implemented"
        emit_scenario_row "P13.6D" "restart_recovery_matrix" "blocked" '"details": {"reason": "slice_not_started"}'
        exit 2
    fi

    if [[ "${P13_6E_RUN:-0}" == 1 ]]; then
        echo "P13.6E: not yet implemented"
        emit_scenario_row "P13.6E" "lost_response_boundary" "blocked" '"details": {"reason": "slice_not_started"}'
        exit 2
    fi

    if [[ "${P13_6F_RUN:-0}" == 1 ]]; then
        echo "P13.6F: not yet implemented"
        emit_scenario_row "P13.6F" "aggregate_closure" "blocked" '"details": {"reason": "slice_not_started"}'
        exit 2
    fi

    echo "P13.6 dispatcher: no slice selected (set P13_6B_RUN .. P13_6F_RUN)"
    echo "P13.6 dispatcher: PASS (skeleton ready)"
}

main "$@"
