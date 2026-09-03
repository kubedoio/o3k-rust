#!/usr/bin/env bash
# P13.6 — Multi-project security and failure evidence
#
# Fail-closed dispatcher for slices B–F, exercised with the contracted
# security/failure matrix from P13.6A.
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
find_free_port() {
    python3 - <<'PY'
import socket
sock = socket.socket()
sock.bind(("127.0.0.1", 0))
print(sock.getsockname()[1])
sock.close()
PY
}

start_o3kd() {
    local state_dir="$1"
    local o3kd_port="$2"
    local extra_args=()
    if [[ -n "${O3K_DATABASE_BACKEND:-}" ]]; then
        extra_args+=(--database-backend "$O3K_DATABASE_BACKEND")
    fi
    if [[ -n "${O3K_DATABASE_URL:-}" ]]; then
        extra_args+=(--database-url "$O3K_DATABASE_URL")
    fi

    mkdir -p "$state_dir"

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
        "${extra_args[@]}" &

    echo $! > "$state_dir/o3kd.pid"

    local attempt
    for attempt in $(seq 1 60); do
        if curl -sf "http://127.0.0.1:$o3kd_port/readyz" >/dev/null 2>&1; then
            return 0
        fi
        if ! kill -0 "$(cat "$state_dir/o3kd.pid")" 2>/dev/null; then
            echo "o3kd exited before becoming ready on port $o3kd_port" >&2
            return 1
        fi
        sleep 0.5
    done
    echo "o3kd failed to become ready on port $o3kd_port" >&2
    return 1
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

get_token() {
    local auth_url="$1"
    local user="$2"
    local user_password="$3"
    local project_name="$4"

    curl -sf -X POST "$auth_url/v3/auth/tokens" \
        -H "Content-Type: application/json" \
        -d "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"$user\",\"password\":\"$user_password\"}}},\"scope\":{\"project\":{\"name\":\"$project_name\"}}}}" \
        -D /tmp/p13-6-token-headers.$$ \
        -o /dev/null 2>/dev/null
    grep -i "^x-subject-token:" /tmp/p13-6-token-headers.$$ | awk '{print $2}' | tr -d '\r'
    rm -f /tmp/p13-6-token-headers.$$
}

# Set up an isolated OpenTofu working directory for one project.
# Arguments: work_dir auth_url tenant_id user_name user_password
setup_tofu_workdir() {
    local work_dir="$1"
    local auth_url="$2"
    local tenant_id="$3"
    local user_name="$4"
    local user_password="$5"

    mkdir -p "$work_dir"

    # Filesystem mirror for offline provider installation
    local mirror_dir="$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
    mkdir -p "$mirror_dir"
    cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"

    cat > "$work_dir/tofu.tfrc" <<TFRC
provider_installation {
  filesystem_mirror {
    path = "${work_dir}/mirror"
  }
  direct {
    exclude = ["terraform-provider-openstack/openstack"]
  }
}
TFRC

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
  auth_url    = "${auth_url}"
  user_name   = "${user_name}"
  password    = "${user_password}"
  tenant_id   = "${tenant_id}"
  max_retries = 0
}
PROV

    (cd "$work_dir" && \
        TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" \
        TF_IN_AUTOMATION=1 \
        "$tofu" init -input=false -upgrade=false -no-color 2>&1 | tail -3)
}

# Emit one machine-readable scenario row as JSON on stdout.
# Extra fields are passed as a JSON object string in $4 (may be empty).
emit_scenario_row() {
    local phase="$1"
    local scenario="$2"
    local result="$3"
    local extra_json="${4:-}"

    local head_sha
    head_sha="$(git -C "$root_dir" rev-parse HEAD 2>/dev/null || echo unknown)"

    P13_6_ROW_PHASE="$phase" \
    P13_6_ROW_SCENARIO="$scenario" \
    P13_6_ROW_RESULT="$result" \
    P13_6_ROW_EXTRA="$extra_json" \
    P13_6_ROW_HEAD_SHA="$head_sha" \
    P13_6_ROW_BACKEND="${O3K_DATABASE_BACKEND:-sqlite}" \
    P13_6_ROW_PROJA_ID="$proja_id" \
    P13_6_ROW_PROJB_ID="$tenb_project" \
    P13_6_ROW_PROJA_PRINCIPAL="$proja_user" \
    P13_6_ROW_PROJB_PRINCIPAL="$tenb_username" \
    python3 - <<'PY'
import json
import os

extra = os.environ.get("P13_6_ROW_EXTRA", "").strip()
row = {
    "phase": os.environ["P13_6_ROW_PHASE"],
    "scenario": os.environ["P13_6_ROW_SCENARIO"],
    "tested_runtime_head_sha": os.environ["P13_6_ROW_HEAD_SHA"],
    "backend": os.environ["P13_6_ROW_BACKEND"],
    "project_a_principal": os.environ["P13_6_ROW_PROJA_PRINCIPAL"],
    "project_a_project": os.environ["P13_6_ROW_PROJA_ID"],
    "project_b_principal": os.environ["P13_6_ROW_PROJB_PRINCIPAL"],
    "project_b_project": os.environ["P13_6_ROW_PROJB_ID"],
    "result": os.environ["P13_6_ROW_RESULT"],
}
if extra:
    row.update(json.loads(extra))
print(json.dumps(row, indent=2))
PY
}

# ---------------------------------------------------------------------------
# Preflight
# ---------------------------------------------------------------------------
preflight() {
    if [[ ! -f "$contract" ]]; then
        echo "P13.6 BLOCKED: contract $contract not found" >&2
        exit 2
    fi

    python3 "$root_dir/scripts/validate_p13_6a_contract.py" || {
        echo "P13.6 BLOCKED: contract validation failed" >&2
        exit 2
    }

    if [[ -z "$tofu" || -z "$provider_binary" || -z "$provider_sha" ]]; then
        echo "P13.6 BLOCKED: set O3K_P13_TOFU, O3K_P13_PROVIDER_BINARY, O3K_P13_PROVIDER_SHA256" >&2
        exit 2
    fi
    local version
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

    python3 "$root_dir/scripts/validate_p13_6a_contract.py"
    echo "P13.6 self-test: contract validation: PASS"

    python3 "$root_dir/scripts/validate_p13_6_evidence.py" --self-test
    echo "P13.6 self-test: evidence schema: PASS"

    python3 "$root_dir/scripts/p13_5e_fault_proxy.py" --self-test
    echo "P13.6 self-test: fault proxy: PASS"

    echo "P13.6 self-test: identity model smoke check"
    local state_dir
    state_dir=$(mktemp -d /tmp/p13-6-smoke-XXXXXX)
    trap 'stop_o3kd "$state_dir" 2>/dev/null || true; rm -rf "$state_dir"' EXIT

    local o3kd_port
    o3kd_port=$(find_free_port)
    start_o3kd "$state_dir" "$o3kd_port"
    local auth_url="http://127.0.0.1:$o3kd_port"

    local token_a
    token_a=$(get_token "$auth_url" "$proja_user" "$password" "$proja_name")
    [[ -n "$token_a" ]] || { echo "P13.6 self-test: FAILED to get token A" >&2; exit 2; }
    echo "P13.6 self-test: token A acquired: OK"

    local token_b
    token_b=$(get_token "$auth_url" "$tenb_username" "$tenb_pass" "$tenb_name")
    [[ -n "$token_b" ]] || { echo "P13.6 self-test: FAILED to get token B" >&2; exit 2; }
    echo "P13.6 self-test: token B acquired: OK"

    if [[ "$token_a" == "$token_b" ]]; then
        echo "P13.6 self-test: FAILED — tokens A and B are identical" >&2
        exit 2
    fi
    echo "P13.6 self-test: tokens A and B are distinct: OK"

    local net_resp net_id
    net_resp=$(curl -sf -X POST "$auth_url/v2.0/networks" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_a" \
        -d '{"network":{"name":"smoke-net"}}')
    net_id=$(printf '%s' "$net_resp" | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])")
    [[ -n "$net_id" ]] || { echo "P13.6 self-test: FAILED — project A network create failed" >&2; exit 2; }
    echo "P13.6 self-test: project A created network $net_id: OK"

    local b_status
    b_status=$(curl -s -o /dev/null -w "%{http_code}" \
        "$auth_url/v2.0/networks/$net_id" \
        -H "X-Auth-Token: $token_b")
    [[ "$b_status" == "404" ]] || {
        echo "P13.6 self-test: FAILED — project B sees project A network (status $b_status)" >&2
        exit 2
    }
    echo "P13.6 self-test: project B cannot access A's network: PASS (404)"

    local a_count b_count
    a_count=$(curl -sf "$auth_url/v2.0/networks" -H "X-Auth-Token: $token_a" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    [[ "$a_count" == "1" ]] || {
        echo "P13.6 self-test: FAILED — expected 1 network for A, got $a_count" >&2
        exit 2
    }
    echo "P13.6 self-test: project A lists 1 network: OK"

    b_count=$(curl -sf "$auth_url/v2.0/networks" -H "X-Auth-Token: $token_b" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    [[ "$b_count" == "0" ]] || {
        echo "P13.6 self-test: FAILED — expected 0 networks for B, got $b_count" >&2
        exit 2
    }
    echo "P13.6 self-test: project B lists 0 networks: OK"

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
        emit_scenario_row "P13.6B" "positive_multiproject_isolation" "blocked" '{"details": {"reason": "slice_not_started"}}'
        exit 2
    fi

    if [[ "${P13_6C_RUN:-0}" == 1 ]]; then
        echo "P13.6C: not yet implemented"
        emit_scenario_row "P13.6C" "cross_project_negative" "blocked" '{"details": {"reason": "slice_not_started"}}'
        exit 2
    fi

    if [[ "${P13_6D_RUN:-0}" == 1 ]]; then
        echo "P13.6D: not yet implemented"
        emit_scenario_row "P13.6D" "restart_recovery_matrix" "blocked" '{"details": {"reason": "slice_not_started"}}'
        exit 2
    fi

    if [[ "${P13_6E_RUN:-0}" == 1 ]]; then
        echo "P13.6E: not yet implemented"
        emit_scenario_row "P13.6E" "lost_response_boundary" "blocked" '{"details": {"reason": "slice_not_started"}}'
        exit 2
    fi

    if [[ "${P13_6F_RUN:-0}" == 1 ]]; then
        echo "P13.6F: not yet implemented"
        emit_scenario_row "P13.6F" "aggregate_closure" "blocked" '{"details": {"reason": "slice_not_started"}}'
        exit 2
    fi

    echo "P13.6 dispatcher: no slice selected (set P13_6B_RUN .. P13_6F_RUN)"
    echo "P13.6 dispatcher: PASS (skeleton ready)"
}

main "$@"
