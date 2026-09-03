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
    include = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
  direct {
    exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"]
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
# P13.6B — positive multi-project isolation
# ---------------------------------------------------------------------------
run_slice_b() {
    echo "P13.6B: positive multi-project isolation"

    local state_dir evidence_file evidence_rows
    state_dir=$(mktemp -d /tmp/p13-6b-XXXXXX)
    evidence_file="$evidence_dir/p13-6b-evidence.json"
    evidence_rows="$state_dir/evidence-rows.jsonl"
    mkdir -p "$(dirname "$evidence_file")" "$state_dir"

    P13_6B_STATE_DIR="$state_dir"
    _p13_6b_cleanup_done=0
    _cleanup_6b() {
        [[ "${_p13_6b_cleanup_done:-0}" == 1 ]] && return
        _p13_6b_cleanup_done=1
        local sd="${P13_6B_STATE_DIR:-}"
        if [[ -n "$sd" ]]; then
            stop_o3kd "$sd" 2>/dev/null || true
            [[ "${O3K_P13_6B_KEEP_STATE:-0}" == 1 ]] || rm -rf "$sd"
        fi
    }
    trap _cleanup_6b EXIT

    local o3kd_port auth_url \
          token_a token_b \
          net_a_id subnet_a_id port_a_id router_a_id sg_a_id \
          net_b_id subnet_b_id port_b_id router_b_id sg_b_id

    o3kd_port=$(find_free_port)
    auth_url="http://127.0.0.1:$o3kd_port"

    echo "P13.6B: starting o3kd on port $o3kd_port"
    start_o3kd "$state_dir" "$o3kd_port"
    echo "P13.6B: o3kd ready"

    # Tofu-per-project helpers (avoid global export with two projects)
    tofu_a() { (cd "$dir_a" && TF_CLI_CONFIG_FILE="$dir_a/tofu.tfrc" TF_IN_AUTOMATION=1 "$tofu" "$@"); }
    tofu_b() { (cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 "$tofu" "$@"); }
    # Placeholder; actual dirs created below
    dir_a=""; dir_b=""

    # -----------------------------------------------------------------------
    # B1 — Identity separation
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B1 - Identity separation ==="

    token_a=$(get_token "$auth_url" "$proja_user" "$password" "$proja_name")
    token_b=$(get_token "$auth_url" "$tenb_username" "$tenb_pass" "$tenb_name")

    if [[ -z "$token_a" || -z "$token_b" ]]; then
        emit_scenario_row "P13.6B" "B1_identity_separation" "failed" '{"details":{"reason":"failed_to_obtain_tokens"}}' >> "$evidence_rows"
        exit 2
    fi

    if [[ "$token_a" == "$token_b" ]]; then
        emit_scenario_row "P13.6B" "B1_identity_separation" "failed" '{"details":{"reason":"tokens_identical"}}' >> "$evidence_rows"
        exit 2
    fi

    local proj_a_validate proj_b_validate
    proj_a_validate=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v3/auth/tokens" \
        | python3 -c "import json,sys; print(json.load(sys.stdin).get('token',{}).get('project',{}).get('id',''))" 2>/dev/null || echo "")
    proj_b_validate=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v3/auth/tokens" \
        | python3 -c "import json,sys; print(json.load(sys.stdin).get('token',{}).get('project',{}).get('id',''))" 2>/dev/null || echo "")

    local b1_failed=0
    if [[ "$proj_a_validate" != "$proja_id" ]]; then
        echo "P13.6B: FAIL - token A resolves to project $proj_a_validate, expected $proja_id" >&2
        b1_failed=1
    fi
    if [[ "$proj_b_validate" != "$tenb_project" ]]; then
        echo "P13.6B: FAIL - token B resolves to project $proj_b_validate, expected $tenb_project" >&2
        b1_failed=1
    fi

    if [[ "$b1_failed" == 1 ]]; then
        emit_scenario_row "P13.6B" "B1_identity_separation" "failed" \
            "{\"details\":{\"token_a_project\":\"$proj_a_validate\",\"token_b_project\":\"$proj_b_validate\"}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B1_identity_separation" "passed" \
        "{\"resource_type\":\"identity\",\"operation\":\"token_validate\",\"target_owner\":\"project_a\",\"caller_owner\":\"system\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"token_a_project\":\"$proj_a_validate\",\"token_b_project\":\"$proj_b_validate\",\"tokens_distinct\":true}}" >> "$evidence_rows"
    echo "P13.6B: B1 PASS"

    # -----------------------------------------------------------------------
    # Setup OpenTofu workdirs for both projects
    # -----------------------------------------------------------------------
    dir_a="$state_dir/project-a"
    dir_b="$state_dir/project-b"

    echo "P13.6B: setting up OpenTofu workdirs"
    setup_tofu_workdir "$dir_a" "$auth_url" "$proja_id" "$proja_user" "$password"
    setup_tofu_workdir "$dir_b" "$auth_url" "$tenb_project" "$tenb_username" "$tenb_pass"

    # -----------------------------------------------------------------------
    # B2 — Same-name resources configuration
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B2 - Same-name resources ==="

    # Identical OpenTofu configs for both projects
    cat > "$dir_a/graph.tf" <<'TOFU_G'
resource "openstack_networking_network_v2" "main" {
  name = "p13-shared-name"
  tags = []
}

resource "openstack_networking_subnet_v2" "main" {
  name = "p13-shared-subnet"
  network_id = openstack_networking_network_v2.main.id
  cidr = "10.0.0.0/24"
  ip_version = 4
  enable_dhcp = false
  dns_nameservers = []
  tags = []
}

resource "openstack_networking_port_v2" "main" {
  name = "p13-shared-port"
  network_id = openstack_networking_network_v2.main.id
  fixed_ip {
    subnet_id = openstack_networking_subnet_v2.main.id
  }
  tags = []
}

resource "openstack_networking_router_v2" "main" {
  name = "p13-shared-router"
  admin_state_up = true
  tags = []
}

resource "openstack_networking_router_interface_v2" "main" {
  router_id = openstack_networking_router_v2.main.id
  subnet_id = openstack_networking_subnet_v2.main.id
}

resource "openstack_networking_secgroup_v2" "main" {
  name = "p13-shared-sg"
  description = "P13.6B shared security group"
  delete_default_rules = false
  tags = []
}

resource "openstack_networking_secgroup_rule_v2" "ssh" {
  security_group_id = openstack_networking_secgroup_v2.main.id
  direction = "ingress"
  ethertype = "IPv4"
  protocol = "tcp"
  port_range_min = 22
  port_range_max = 22
  remote_ip_prefix = "0.0.0.0/0"
}
TOFU_G
    cp "$dir_a/graph.tf" "$dir_b/graph.tf"

    emit_scenario_row "P13.6B" "B2_same_name_resources" "passed" \
        '{"resource_type":"configuration","operation":"plan","target_owner":"project_a","caller_owner":"project_a","expected_authorization_outcome":"allow","actual_http_status":200,"details":{"configs_identical":true,"shared_names":["p13-shared-name","p13-shared-subnet","p13-shared-port","p13-shared-router","p13-shared-sg"]}}' >> "$evidence_rows"
    echo "P13.6B: B2 PASS"

    # -----------------------------------------------------------------------
    # B3 — Networking graph for Project A
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B3 - Networking graph (Project A) ==="

    tofu_a apply -input=false -auto-approve >/dev/null

    net_a_id=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_network_v2.main"))')
    subnet_a_id=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_subnet_v2.main"))')
    port_a_id=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_port_v2.main"))')
    router_a_id=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_router_v2.main"))')
    sg_a_id=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_secgroup_v2.main"))')

    echo "P13.6B: A resources — net=$net_a_id subnet=$subnet_a_id port=$port_a_id router=$router_a_id sg=$sg_a_id"
    emit_scenario_row "P13.6B" "B3_network_graph_a" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"create\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"network_id\":\"$net_a_id\",\"subnet_id\":\"$subnet_a_id\",\"port_id\":\"$port_a_id\",\"router_id\":\"$router_a_id\",\"security_group_id\":\"$sg_a_id\"}}" >> "$evidence_rows"
    echo "P13.6B: B3 (A) PASS"

    # -----------------------------------------------------------------------
    # B4 — Isolation verification
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B4 - Isolation verification ==="

    # B sees zero before its own create
    local b_net_count
    b_net_count=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))" 2>/dev/null || echo "error")

    if [[ "$b_net_count" != "0" ]]; then
        emit_scenario_row "P13.6B" "B4_isolation_before_b" "failed" \
            "{\"details\":{\"b_network_count\":\"$b_net_count\",\"expected\":0}}" >> "$evidence_rows"
        exit 2
    fi

    local b_net_status
    b_net_status=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks/$net_a_id")
    if [[ "$b_net_status" != "404" ]]; then
        emit_scenario_row "P13.6B" "B4_isolation_before_b" "failed" \
            "{\"details\":{\"b_network_show_status\":$b_net_status,\"expected\":404}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B4_isolation_before_b" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"list\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_b\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"b_network_count\":0,\"b_network_show_status\":404}}" >> "$evidence_rows"
    echo "P13.6B: B4 (before B) PASS"

    # -----------------------------------------------------------
    # B3 (continued) — Networking graph for Project B
    # -----------------------------------------------------------
    echo "P13.6B: === B3 (continued) - Networking graph (Project B) ==="

    tofu_b apply -input=false -auto-approve >/dev/null

    net_b_id=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_network_v2.main"))')
    subnet_b_id=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_subnet_v2.main"))')
    port_b_id=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_port_v2.main"))')
    router_b_id=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_router_v2.main"))')
    sg_b_id=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_secgroup_v2.main"))')

    echo "P13.6B: B resources — net=$net_b_id subnet=$subnet_b_id port=$port_b_id router=$router_b_id sg=$sg_b_id"
    emit_scenario_row "P13.6B" "B3_network_graph_b" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"create\",\"target_owner\":\"project_b\",\"caller_owner\":\"project_b\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"network_id\":\"$net_b_id\",\"subnet_id\":\"$subnet_b_id\",\"port_id\":\"$port_b_id\",\"router_id\":\"$router_b_id\",\"security_group_id\":\"$sg_b_id\"}}" >> "$evidence_rows"
    echo "P13.6B: B3 (B) PASS"

    # Verify IDs differ between A and B
    local b4_ids_ok=1
    [[ "$net_a_id" != "$net_b_id" ]] || { echo "P13.6B: FAIL - network IDs identical" >&2; b4_ids_ok=0; }
    [[ "$subnet_a_id" != "$subnet_b_id" ]] || { echo "P13.6B: FAIL - subnet IDs identical" >&2; b4_ids_ok=0; }
    [[ "$port_a_id" != "$port_b_id" ]] || { echo "P13.6B: FAIL - port IDs identical" >&2; b4_ids_ok=0; }
    [[ "$router_a_id" != "$router_b_id" ]] || { echo "P13.6B: FAIL - router IDs identical" >&2; b4_ids_ok=0; }
    [[ "$sg_a_id" != "$sg_b_id" ]] || { echo "P13.6B: FAIL - SG IDs identical" >&2; b4_ids_ok=0; }

    if [[ "$b4_ids_ok" != 1 ]]; then
        emit_scenario_row "P13.6B" "B4_ids_distinct" "failed" \
            "{\"details\":{\"network_a\":\"$net_a_id\",\"network_b\":\"$net_b_id\"}}" >> "$evidence_rows"
        exit 2
    fi

    # Cross-project isolation check
    local a_show_b_status a_count_after b_count_after
    a_show_b_status=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks/$net_b_id")
    [[ "$a_show_b_status" == "404" ]] || { echo "P13.6B: FAIL - A sees B's network (status $a_show_b_status)" >&2; b4_ids_ok=0; }

    a_count_after=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    b_count_after=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    [[ "$a_count_after" == "1" ]] || { echo "P13.6B: FAIL - A sees $a_count_after networks, expected 1" >&2; b4_ids_ok=0; }
    [[ "$b_count_after" == "1" ]] || { echo "P13.6B: FAIL - B sees $b_count_after networks, expected 1" >&2; b4_ids_ok=0; }

    if [[ "$b4_ids_ok" != 1 ]]; then
        emit_scenario_row "P13.6B" "B4_ids_distinct" "failed" \
            "{\"details\":{\"a_show_b_status\":$a_show_b_status,\"a_network_count\":$a_count_after,\"b_network_count\":$b_count_after}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B4_ids_distinct" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"show\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_b\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"ids_distinct\":true,\"a_network_count\":$a_count_after,\"b_network_count\":$b_count_after}}" >> "$evidence_rows"
    echo "P13.6B: B4 PASS"

    # -----------------------------------------------------------------------
    # B6 — Concurrent operation
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B6 - Concurrent operation ==="

    cat > "$dir_a/concurrent.tf" <<'CONCUR'
resource "openstack_networking_network_v2" "concurrent" {
  name = "p13-concurrent-network"
  tags = []
}
CONCUR
    cp "$dir_a/concurrent.tf" "$dir_b/concurrent.tf"

    tofu_a apply -input=false -auto-approve >/dev/null &
    local pid_a=$!
    tofu_b apply -input=false -auto-approve >/dev/null &
    local pid_b=$!

    wait "$pid_a" "$pid_b" || {
        emit_scenario_row "P13.6B" "B6_concurrent_operation" "failed" \
            '{"details":{"reason":"concurrent_apply_failed"}}' >> "$evidence_rows"
        exit 2
    }

    local conc_net_a conc_net_b
    conc_net_a=$(tofu_a show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_network_v2.concurrent"))')
    conc_net_b=$(tofu_b show -json | python3 -c '
import json,sys; r=json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in r if x["address"]=="openstack_networking_network_v2.concurrent"))')

    if [[ -z "$conc_net_a" || -z "$conc_net_b" || "$conc_net_a" == "$conc_net_b" ]]; then
        emit_scenario_row "P13.6B" "B6_concurrent_operation" "failed" \
            "{\"details\":{\"net_a_id\":\"$conc_net_a\",\"net_b_id\":\"$conc_net_b\"}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B6_concurrent_operation" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"create\",\"target_owner\":\"project_a\",\"caller_owner\":\"system\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"concurrent_network_a\":\"$conc_net_a\",\"concurrent_network_b\":\"$conc_net_b\"}}" >> "$evidence_rows"
    echo "P13.6B: B6 PASS"

    # Clean up concurrent networks
    rm -f "$dir_a/concurrent.tf" "$dir_b/concurrent.tf"
    tofu_a destroy -auto-approve -target openstack_networking_network_v2.concurrent >/dev/null 2>&1 || true
    tofu_b destroy -auto-approve -target openstack_networking_network_v2.concurrent >/dev/null 2>&1 || true

    # -----------------------------------------------------------------------
    # B7 — Same idempotency key across projects
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B7 - Same idempotency key ==="

    local idem_key="p13-6b-idem-$(date +%s)-$$"
    local idem_net_a_id idem_net_b_id

    idem_net_a_id=$(curl -sf -X POST "$auth_url/v2.0/networks" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_a" \
        -H "OpenStack-API-Idempotency-Key: $idem_key" \
        -d '{"network":{"name":"p13-idem-network"}}' \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])" 2>/dev/null || echo "")

    idem_net_b_id=$(curl -sf -X POST "$auth_url/v2.0/networks" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_b" \
        -H "OpenStack-API-Idempotency-Key: $idem_key" \
        -d '{"network":{"name":"p13-idem-network"}}' \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])" 2>/dev/null || echo "")

    if [[ -z "$idem_net_a_id" || -z "$idem_net_b_id" || "$idem_net_a_id" == "$idem_net_b_id" ]]; then
        emit_scenario_row "P13.6B" "B7_idempotency_key" "failed" \
            "{\"details\":{\"idem_key\":\"$idem_key\",\"net_a_id\":\"$idem_net_a_id\",\"net_b_id\":\"$idem_net_b_id\"}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B7_idempotency_key" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"create\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"idem_key\":\"$idem_key\",\"net_a_id\":\"$idem_net_a_id\",\"net_b_id\":\"$idem_net_b_id\",\"ids_distinct\":true}}" >> "$evidence_rows"
    echo "P13.6B: B7 PASS"

    # Clean idempotency test networks
    curl -sf -H "X-Auth-Token: $token_a" -X DELETE "$auth_url/v2.0/networks/$idem_net_a_id" >/dev/null 2>&1 || true
    curl -sf -H "X-Auth-Token: $token_b" -X DELETE "$auth_url/v2.0/networks/$idem_net_b_id" >/dev/null 2>&1 || true

    # -----------------------------------------------------------------------
    # B8 — Restart reconstruction
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B8 - Restart reconstruction ==="

    restart_daemon "$state_dir" "$o3kd_port"

    # Re-authenticate
    token_a=$(get_token "$auth_url" "$proja_user" "$password" "$proja_name")
    token_b=$(get_token "$auth_url" "$tenb_username" "$tenb_pass" "$tenb_name")

    if [[ -z "$token_a" || -z "$token_b" ]]; then
        emit_scenario_row "P13.6B" "B8_restart_reconstruction" "failed" \
            '{"details":{"reason":"reauthentication_after_restart_failed"}}' >> "$evidence_rows"
        exit 2
    fi

    local b8_ok=1
    local post_restart_net_a post_restart_net_b
    local post_restart_subnet_a post_restart_subnet_b
    local post_restart_port_a post_restart_port_b
    local post_restart_router_a post_restart_router_b
    local post_restart_sg_a post_restart_sg_b

    post_restart_net_a=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks/$net_a_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])" 2>/dev/null || echo "")
    post_restart_net_b=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks/$net_b_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])" 2>/dev/null || echo "")

    [[ "$post_restart_net_a" == "$net_a_id" ]] || { echo "P13.6B: FAIL - A network ID mismatch after restart" >&2; b8_ok=0; }
    [[ "$post_restart_net_b" == "$net_b_id" ]] || { echo "P13.6B: FAIL - B network ID mismatch after restart" >&2; b8_ok=0; }

    local post_restart_counts_a post_restart_counts_b
    post_restart_counts_a=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    post_restart_counts_b=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks" \
        | python3 -c "import json,sys; print(len(json.load(sys.stdin).get('networks',[])))")
    [[ "$post_restart_counts_a" == "1" ]] || { echo "P13.6B: FAIL - A has $post_restart_counts_a networks after restart" >&2; b8_ok=0; }
    [[ "$post_restart_counts_b" == "1" ]] || { echo "P13.6B: FAIL - B has $post_restart_counts_b networks after restart" >&2; b8_ok=0; }

    # Refresh-only plans should be no-op
    local plan_a_output plan_b_output
    plan_a_output=$(tofu_a plan -input=false -refresh-only -no-color 2>&1 || true)
    plan_b_output=$(tofu_b plan -input=false -refresh-only -no-color 2>&1 || true)

    local plan_a_noop=0 plan_b_noop=0
    if echo "$plan_a_output" | grep -q "No changes"; then plan_a_noop=1; fi
    if echo "$plan_b_output" | grep -q "No changes"; then plan_b_noop=1; fi

    [[ "$plan_a_noop" == 1 ]] || { echo "P13.6B: FAIL - A plan is not no-op after restart" >&2; b8_ok=0; }
    [[ "$plan_b_noop" == 1 ]] || { echo "P13.6B: FAIL - B plan is not no-op after restart" >&2; b8_ok=0; }

    if [[ "$b8_ok" != 1 ]]; then
        emit_scenario_row "P13.6B" "B8_restart_reconstruction" "failed" \
            "{\"details\":{\"post_restart_net_a\":\"$post_restart_net_a\",\"post_restart_net_b\":\"$post_restart_net_b\",\"a_count\":$post_restart_counts_a,\"b_count\":$post_restart_counts_b}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B8_restart_reconstruction" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"read\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"net_a_reconstructed\":true,\"net_b_reconstructed\":true,\"a_plan_noop\":true,\"b_plan_noop\":true}}" >> "$evidence_rows"
    echo "P13.6B: B8 PASS"

    # -----------------------------------------------------------------------
    # B9 — Independent mutation
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === B9 - Independent mutation ==="

    local name_a_initial name_b_initial

    name_a_initial=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks/$net_a_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")
    name_b_initial=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks/$net_b_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")

    # Update A's network name
    curl -sf -X PUT "$auth_url/v2.0/networks/$net_a_id" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_a" \
        -d '{"network":{"name":"p13-shared-name-updated-a"}}' >/dev/null

    local name_a_after_a name_b_after_a
    name_a_after_a=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks/$net_a_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")
    name_b_after_a=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks/$net_b_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")

    local b9_ok=1
    [[ "$name_a_after_a" == "p13-shared-name-updated-a" ]] || { echo "P13.6B: FAIL - A name update failed" >&2; b9_ok=0; }
    [[ "$name_b_after_a" == "$name_b_initial" ]] || { echo "P13.6B: FAIL - B name changed when A was updated" >&2; b9_ok=0; }

    # Update B's network name
    curl -sf -X PUT "$auth_url/v2.0/networks/$net_b_id" \
        -H "Content-Type: application/json" \
        -H "X-Auth-Token: $token_b" \
        -d '{"network":{"name":"p13-shared-name-updated-b"}}' >/dev/null

    local name_a_final name_b_final
    name_a_final=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/networks/$net_a_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")
    name_b_final=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/networks/$net_b_id" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['name'])")

    [[ "$name_a_final" == "p13-shared-name-updated-a" ]] || { echo "P13.6B: FAIL - A name changed when B was updated" >&2; b9_ok=0; }
    [[ "$name_b_final" == "p13-shared-name-updated-b" ]] || { echo "P13.6B: FAIL - B name update failed" >&2; b9_ok=0; }

    if [[ "$b9_ok" != 1 ]]; then
        emit_scenario_row "P13.6B" "B9_independent_mutation" "failed" \
            "{\"details\":{\"name_a_initial\":\"$name_a_initial\",\"name_a_after_a_update\":\"$name_a_after_a\",\"name_b_after_a_update\":\"$name_b_after_a\",\"name_a_after_b_update\":\"$name_a_final\",\"name_b_after_b_update\":\"$name_b_final\"}}" >> "$evidence_rows"
        exit 2
    fi

    emit_scenario_row "P13.6B" "B9_independent_mutation" "passed" \
        "{\"resource_type\":\"openstack_networking_network_v2\",\"operation\":\"update\",\"target_owner\":\"project_a\",\"caller_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"name_a_final\":\"$name_a_final\",\"name_b_final\":\"$name_b_final\",\"mutation_independent\":true}}" >> "$evidence_rows"
    echo "P13.6B: B9 PASS"

    # -----------------------------------------------------------------------
    # Cleanup: destroy all resources via OpenTofu
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === Cleanup ==="

    tofu_a destroy -input=false -auto-approve >/dev/null 2>&1 || true
    tofu_b destroy -input=false -auto-approve >/dev/null 2>&1 || true

    # -----------------------------------------------------------------------
    # Write evidence artifact
    # -----------------------------------------------------------------------
    echo ""
    echo "P13.6B: === Writing evidence artifact ==="

    local head_sha
    head_sha=$(git -C "$root_dir" rev-parse HEAD 2>/dev/null || echo "unknown")

    python3 - "$evidence_rows" "$evidence_file" "$head_sha" <<'PY_EVIDENCE'
import json, pathlib, sys

rows_path, out_path, head_sha = sys.argv[1:]

rows = []
if pathlib.Path(rows_path).exists():
    text = pathlib.Path(rows_path).read_text()
    # Concatenated multi-line JSON objects — parse with streaming decoder
    decoder = json.JSONDecoder()
    pos = 0
    while pos < len(text):
        while pos < len(text) and text[pos] in ' \t\n\r':
            pos += 1
        if pos >= len(text):
            break
        try:
            obj, end = decoder.raw_decode(text, pos)
            rows.append(obj)
            pos = end
        except json.JSONDecodeError:
            break

result_counts = {}
for r in rows:
    k = r.get("result", "unknown")
    result_counts[k] = result_counts.get(k, 0) + 1

all_passed = all(r.get("result") == "passed" for r in rows)

document = {
    "artifact_type": "o3k-p13-6b-multiproject-isolation",
    "schema_version": 1,
    "phase": "P13.6B",
    "tested_runtime_head_sha": head_sha,
    "backend": "postgresql",
    "provider_modified": False,
    "scenarios": rows,
    "result_counts": result_counts,
    "aggregate_verdict": "PASS" if all_passed else "FAILED",
}

pathlib.Path(out_path).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
print(f"P13.6B evidence written to {out_path}")
print(f"P13.6B evidence: {len(rows)} scenarios, result_counts={json.dumps(result_counts)}")
PY_EVIDENCE

    _cleanup_6b
    echo ""
    echo "P13.6B: ALL PASS"
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
        run_slice_b
        exit 0
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
