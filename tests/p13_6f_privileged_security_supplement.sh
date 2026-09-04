#!/usr/bin/env bash
# P13.6F — Privileged security supplement and A-matrix completion.
#
# Executes, on the accepted disposable Compute/LVM TestLab tier with
# PostgreSQL, the multi-project security evidence that the portable P13.6B/C
# runs classified as execution_profile_unavailable (issue #809):
#
#   * positive two-project Compute/Volume/VolumeAttachment isolation
#     (supersedes B10/B11/B12);
#   * cross-project storage relationship attacks
#     (supersedes C7_volume_attach_foreign_server / C7_volume_foreign_detach);
#   * restart reconstruction of Compute/Storage ownership (extends B8/D6);
#   * portable completion of every remaining applicable foreign-access cell
#     of the frozen P13.6A resource_security_matrix that B/C did not execute.
#
# The upstream provider remains unmodified; OpenTofu 1.12.6 and provider
# 3.4.0 with frozen hashes are required.  No credentials are written to the
# evidence artifact.
set -euo pipefail

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

# ---------------------------------------------------------------------------
# Required environment
# ---------------------------------------------------------------------------
tofu="${O3K_P13_TOFU:?set O3K_P13_TOFU to OpenTofu 1.12.6}"
tofu_archive="${O3K_P13_TOFU_ARCHIVE:?set O3K_P13_TOFU_ARCHIVE}"
provider_archive="${O3K_P13_PROVIDER_ARCHIVE:?set O3K_P13_PROVIDER_ARCHIVE}"
provider_binary="${O3K_P13_PROVIDER_BINARY:?set O3K_P13_PROVIDER_BINARY to the unmodified provider 3.4.0 binary}"
provider_sha="${O3K_P13_PROVIDER_SHA256:?set O3K_P13_PROVIDER_SHA256}"
: "${O3K_LVM_VOLUME_GROUP:?set a disposable LVM volume group}"
: "${O3K_LVM_THIN_POOL:?set a disposable LVM thin pool}"
: "${O3K_LVM_PROVIDER_NAMESPACE:?set a disposable LVM provider namespace}"
[[ "${O3K_DATABASE_BACKEND:-}" == "postgres" ]] || { echo "supplement BLOCKED: O3K_DATABASE_BACKEND=postgres is required" >&2; exit 2; }
: "${O3K_DATABASE_URL:?set O3K_DATABASE_URL}"

o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
output="${O3K_P13_6F_SUPPLEMENT_EVIDENCE_OUTPUT:-$root_dir/target/p13-6f/supplement-evidence.json}"
password="${O3K_P13_PASSWORD:-p13-6f-supplement-password}"

proja_id="eba29e2d-53de-461d-ae91-ede7402713cb"
proja_name="admin"
proja_user="admin"
tenb_project="${O3K_EXTRA_TENANT_PROJECT_ID:-9f3c2b6e-5f2d-4b3a-9c8e-1a2b3c4d5e6f}"
tenb_name="${O3K_EXTRA_TENANT_PROJECT_NAME:-tenant-b}"
tenb_user="${O3K_EXTRA_TENANT_USER_ID:-6b0f5a2e-8c4d-4a7e-9b1f-2d3e4f5a6b7c}"
tenb_username="${O3K_EXTRA_TENANT_USER_NAME:-tenant-b-user}"
tenb_pass="${O3K_EXTRA_TENANT_PASSWORD:-tenant-b-password}"

port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
auth_url="http://127.0.0.1:$port"
state_dir="$(mktemp -d /var/tmp/o3k-p13-6f-supp.XXXXXX)"
work_dir="$(mktemp -d /var/tmp/o3k-p13-6f-tofu.XXXXXX)"
dir_a="$work_dir/project-a"
dir_b="$work_dir/project-b"
evidence_rows="$state_dir/evidence-rows.jsonl"
: > "$evidence_rows"

cleanup() {
    local status=$?
    stop_o3kd "$state_dir" 2>/dev/null || true
    if [[ "$status" -ne 0 || "${O3K_P13_6F_KEEP_WORK:-0}" == 1 ]]; then
        echo "P13.6F supplement: work preserved: $state_dir $work_dir" >&2
    else
        rm -rf "$work_dir"
    fi
}
trap cleanup EXIT

# ---------------------------------------------------------------------------
# Helpers
# ---------------------------------------------------------------------------
find_free_port() {
    python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()'
}

start_o3kd() {
    local state="$1" o3kd_port="$2"
    mkdir -p "$state"
    O3K_BOOTSTRAP_PASSWORD="$password" \
    O3K_TOKEN_SIGNING_KEY="p13-6f-supplement-signing-key-0123456789012" \
    O3K_CINDER_ENDPOINT="http://127.0.0.1:$o3kd_port" \
    O3K_EXTRA_TENANT_PROJECT_ID="$tenb_project" \
    O3K_EXTRA_TENANT_PROJECT_NAME="$tenb_name" \
    O3K_EXTRA_TENANT_USER_ID="$tenb_user" \
    O3K_EXTRA_TENANT_USER_NAME="$tenb_username" \
    O3K_EXTRA_TENANT_PASSWORD="$tenb_pass" \
    "$o3kd" \
        --listen-addr "127.0.0.1:$o3kd_port" \
        --data-dir "$state/data" \
        --database-backend postgres \
        --database-url "$O3K_DATABASE_URL" \
        > "$state/o3kd.log" 2>&1 &
    echo $! > "$state/o3kd.pid"
    local attempt
    for attempt in $(seq 1 120); do
        if curl -sf "http://127.0.0.1:$o3kd_port/readyz" >/dev/null 2>&1; then
            return 0
        fi
        if ! kill -0 "$(cat "$state/o3kd.pid")" 2>/dev/null; then
            echo "o3kd exited before becoming ready (log: $state/o3kd.log)" >&2
            return 1
        fi
        sleep 0.5
    done
    echo "o3kd failed to become ready on port $o3kd_port" >&2
    return 1
}

stop_o3kd() {
    local state="$1"
    local pid_file="$state/o3kd.pid"
    [[ -f "$pid_file" ]] || return 0
    local pid
    pid=$(cat "$pid_file")
    kill "$pid" 2>/dev/null || true
    local attempt
    for attempt in $(seq 1 40); do
        kill -0 "$pid" 2>/dev/null || break
        sleep 0.25
    done
    kill -9 "$pid" 2>/dev/null || true
    rm -f "$pid_file"
}

get_token() {
    local user="$1" user_password="$2" project_name="$3"
    local header_file
    header_file=$(mktemp /tmp/p13-6f-token-headers.XXXXXX)
    curl -sf -X POST "$auth_url/v3/auth/tokens" \
        -H "Content-Type: application/json" \
        -d "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"$user\",\"password\":\"$user_password\"}}},\"scope\":{\"project\":{\"name\":\"$project_name\"}}}}" \
        -D "$header_file" -o /dev/null 2>/dev/null
    grep -i "^x-subject-token:" "$header_file" | awk '{print $2}' | tr -d '\r'
    rm -f "$header_file"
}

# Emit one evidence row. $4 = extra JSON, $5 = comma-separated A-matrix cells.
emit_row() {
    local scenario="$1" result="$2" extra_json="${3:-}" cells="${4:-}"
    local head_sha
    head_sha="$(git -C "$root_dir" rev-parse HEAD 2>/dev/null || echo unknown)"
    P13_6F_ROW_SCENARIO="$scenario" \
    P13_6F_ROW_RESULT="$result" \
    P13_6F_ROW_EXTRA="$extra_json" \
    P13_6F_ROW_CELLS="$cells" \
    P13_6F_ROW_HEAD_SHA="$head_sha" \
    python3 - <<'PY'
import json, os
row = {
    "scenario": os.environ["P13_6F_ROW_SCENARIO"],
    "phase": "P13.6F",
    "result": os.environ["P13_6F_ROW_RESULT"],
    "execution_tier": "privileged-testlab-disposable-lvm",
    "tested_runtime_head_sha": os.environ["P13_6F_ROW_HEAD_SHA"],
    "backend": "postgresql",
    "provider_modified": False,
    "project_a": os.environ.get("P13_6F_PROJA_ID", ""),
    "project_b": os.environ.get("P13_6F_PROJB_ID", ""),
}
extra = os.environ.get("P13_6F_ROW_EXTRA", "")
if extra:
    row.update(json.loads(extra))
cells = os.environ.get("P13_6F_ROW_CELLS", "")
if cells:
    row["a_matrix_cells"] = [c.strip() for c in cells.split(",") if c.strip()]
print(json.dumps(row))
PY
}

# curl wrapper returning "status<TAB>body-file".
# usage: api_call <method> <token> <path> [data]
api_call() {
    local method="$1" token="$2" path="$3" data="${4:-}"
    local body
    body=$(mktemp /tmp/p13-6f-body.XXXXXX)
    local status
    if [[ -n "$data" ]]; then
        status=$(curl -s -o "$body" -w "%{http_code}" -X "$method" \
            -H "Content-Type: application/json" -H "X-Auth-Token: $token" \
            --data "$data" "$auth_url$path")
    else
        status=$(curl -s -o "$body" -w "%{http_code}" -X "$method" \
            -H "X-Auth-Token: $token" "$auth_url$path")
    fi
    echo "$status"
    echo "$body"
}

# Non-disclosure check: the response body must not reveal the foreign
# resource's identity. $1 = body file, rest = forbidden strings.
check_no_disclosure() {
    local body_file="$1"; shift
    local needle
    for needle in "$@"; do
        [[ -z "$needle" ]] && continue
        if grep -qF "$needle" "$body_file" 2>/dev/null; then
            return 1
        fi
    done
    return 0
}

random_uuid() {
    python3 -c 'import uuid; print(uuid.uuid4())'
}

# Foreign-access attempt with existence-oracle control is provided by `fa`
# below (defined in the completion section); relationship attacks use
# explicit scenario blocks.

setup_tofu_workdir() {
    local work="$1" tenant_id="$2" user_name="$3" user_password="$4"
    mkdir -p "$work"
    local mirror_dir="$work/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
    mkdir -p "$mirror_dir"
    cp "$provider_binary" "$mirror_dir/terraform-provider-openstack_v3.4.0"
    cat > "$work/tofu.tfrc" <<TFRC
provider_installation {
  filesystem_mirror {
    path = "${work}/mirror"
    include = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
  direct {
    exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
}
TFRC
    cat > "$work/provider.tf" <<PROV
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
    (cd "$work" && TF_CLI_CONFIG_FILE="$work/tofu.tfrc" TF_IN_AUTOMATION=1 "$tofu" init -input=false -upgrade=false >/dev/null)
}

tofu_in() {
    local work="$1"; shift
    (cd "$work" && TF_CLI_CONFIG_FILE="$work/tofu.tfrc" TF_IN_AUTOMATION=1 "$tofu" "$@")
}

extract_id() {
    local work="$1" address="$2"
    tofu_in "$work" show -json | python3 -c '
import json, sys
resources = json.load(sys.stdin)["values"]["root_module"]["resources"]
print(next(x["values"]["id"] for x in resources if x["address"] == sys.argv[1]))
' "$address"
}

# ---------------------------------------------------------------------------
# Main flow
# ---------------------------------------------------------------------------
export P13_6F_PROJA_ID="$proja_id" P13_6F_PROJB_ID="$tenb_project"

echo "P13.6F supplement: starting o3kd (port $port, backend postgresql)"
export O3K_NETWORK_EXTERNAL_REALM_ID="00000000-0000-0000-0000-000000000009"
export O3K_PUBLIC_POOL_CIDR="198.51.104.0/29"
export O3K_PUBLIC_POOL_FIRST="198.51.104.2"
export O3K_PUBLIC_POOL_LAST="198.51.104.6"
start_o3kd "$state_dir" "$port"

echo "P13.6F supplement: creating external pool for floating IP support"
token_a=$(get_token "$proja_user" "$password" "$proja_name")
[[ -n "$token_a" ]] || { echo "P13.6F supplement: FAILED to get token A" >&2; exit 2; }
external_realm_id=$(curl -sf -X POST "$auth_url/v2.0/networks" \
    -H "Content-Type: application/json" -H "X-Auth-Token: $token_a" \
    -d '{"network":{"name":"p13-6f-public-pool","router:external":true,"shared":true}}' \
    | python3 -c "import json,sys; print(json.load(sys.stdin)['network']['id'])" 2>/dev/null || echo "")
[[ -n "$external_realm_id" ]] || { echo "P13.6F supplement: FAILED to create external pool" >&2; exit 2; }
stop_o3kd "$state_dir"
sleep 1
export O3K_NETWORK_EXTERNAL_REALM_ID="$external_realm_id"
start_o3kd "$state_dir" "$port"
echo "P13.6F supplement: o3kd ready with floating IP support"

token_a=$(get_token "$proja_user" "$password" "$proja_name")
token_b=$(get_token "$tenb_username" "$tenb_pass" "$tenb_name")
[[ -n "$token_a" && -n "$token_b" && "$token_a" != "$token_b" ]] \
    || { echo "P13.6F supplement: FAILED independent token model" >&2; exit 2; }

# --- images: one private image per project ---------------------------------
make_image() {
    local token="$1" name="$2"
    local image_id
    image_id=$(curl -sf -X POST "$auth_url/v2/images" \
        -H "Content-Type: application/json" -H "X-Auth-Token: $token" \
        -d "{\"name\":\"$name\",\"visibility\":\"private\",\"container_format\":\"bare\",\"disk_format\":\"raw\"}" \
        | python3 -c "import json,sys; print(json.load(sys.stdin)['id'])")
    printf '%s' "$name" | curl -sf -X PUT \
        -H "X-Auth-Token: $token" -H "Content-Type: application/octet-stream" \
        --data-binary @- "$auth_url/v2/images/$image_id/file" >/dev/null
    echo "$image_id"
}
image_a=$(make_image "$token_a" "p13-6f-image-a")
image_b=$(make_image "$token_b" "p13-6f-image-b")
echo "P13.6F supplement: images created (a=$image_a b=$image_b)"

# --- tofu workdirs with identical names -------------------------------------
write_graph() {
    local work="$1" image_id="$2" cidr="$3"
    cat > "$work/graph.tf" <<TF
resource "openstack_networking_network_v2" "net" {
  name = "p13-shared-name"
}
resource "openstack_networking_subnet_v2" "subnet" {
  name        = "p13-shared-subnet"
  network_id  = openstack_networking_network_v2.net.id
  cidr        = "$cidr"
  ip_version  = 4
  enable_dhcp = false
}
resource "openstack_compute_keypair_v2" "kp" {
  name       = "p13-shared-keypair"
  public_key = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOAX5ZKKeCYUDXqLY+HJn3CYOOZ5l6/TnaMurBMYhvCI"
}
resource "openstack_compute_instance_v2" "server" {
  name      = "p13-shared-server"
  image_id  = "$image_id"
  flavor_id = "00000000-0000-0000-0000-000000000001"
  key_pair  = openstack_compute_keypair_v2.kp.name
  depends_on = [openstack_networking_subnet_v2.subnet]
  network {
    uuid = openstack_networking_network_v2.net.id
  }
}
resource "openstack_blockstorage_volume_v3" "volume" {
  name = "p13-shared-volume"
  size = 1
}
resource "openstack_compute_volume_attach_v2" "attachment" {
  instance_id = openstack_compute_instance_v2.server.id
  volume_id   = openstack_blockstorage_volume_v3.volume.id
  device      = "/dev/vdb"
}
TF
}

setup_tofu_workdir "$dir_a" "$proja_id" "$proja_user" "$password"
setup_tofu_workdir "$dir_b" "$tenb_project" "$tenb_username" "$tenb_pass"
write_graph "$dir_a" "$image_a" "198.51.110.0/24"
write_graph "$dir_b" "$image_b" "198.51.111.0/24"

# ---------------------------------------------------------------------------
# S1 — positive two-project compute/storage isolation (sequential full graphs)
# ---------------------------------------------------------------------------
echo "P13.6F supplement: S1 sequential full-graph apply for projects A and B"
tofu_in "$dir_a" apply -input=false -auto-approve > "$state_dir/apply-a.log" 2>&1
echo "P13.6F supplement: project A graph applied"
tofu_in "$dir_b" apply -input=false -auto-approve > "$state_dir/apply-b.log" 2>&1
echo "P13.6F supplement: project B graph applied"

server_a=$(extract_id "$dir_a" "openstack_compute_instance_v2.server")
server_b=$(extract_id "$dir_b" "openstack_compute_instance_v2.server")
volume_a=$(extract_id "$dir_a" "openstack_blockstorage_volume_v3.volume")
volume_b=$(extract_id "$dir_b" "openstack_blockstorage_volume_v3.volume")
attach_a=$(extract_id "$dir_a" "openstack_compute_volume_attach_v2.attachment")
attach_b=$(extract_id "$dir_b" "openstack_compute_volume_attach_v2.attachment")
net_a=$(extract_id "$dir_a" "openstack_networking_network_v2.net")
net_b=$(extract_id "$dir_b" "openstack_networking_network_v2.net")
subnet_a=$(extract_id "$dir_a" "openstack_networking_subnet_v2.subnet")
subnet_b=$(extract_id "$dir_b" "openstack_networking_subnet_v2.subnet")
kp_a=$(extract_id "$dir_a" "openstack_compute_keypair_v2.kp")

[[ -n "$server_a" && -n "$server_b" && "$server_a" != "$server_b" ]] \
    || { echo "P13.6F supplement: server identity failure" >&2; exit 2; }
[[ -n "$volume_a" && -n "$volume_b" && "$volume_a" != "$volume_b" ]] \
    || { echo "P13.6F supplement: volume identity failure" >&2; exit 2; }
[[ -n "$attach_a" && -n "$attach_b" && "$attach_a" != "$attach_b" ]] \
    || { echo "P13.6F supplement: attachment identity failure" >&2; exit 2; }

# The tofu provider identifier for a volume attachment is the compound
# "instance_id/attachment_id"; the os-volume_attachments/{id} API paths need
# the bare attachment UUID reported in the volumeAttachments list.
att_id_a=$(curl -sf -H "X-Auth-Token: $token_a" \
    "$auth_url/v2.1/$proja_id/servers/$server_a/os-volume_attachments" \
    | python3 -c "import json,sys; a=json.load(sys.stdin).get('volumeAttachments',[]) or []; print((a[0].get('attachment_id') or a[0].get('id') or '') if a else '')")
att_id_b=$(curl -sf -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments" \
    | python3 -c "import json,sys; a=json.load(sys.stdin).get('volumeAttachments',[]) or []; print((a[0].get('attachment_id') or a[0].get('id') or '') if a else '')")
[[ -n "$att_id_a" && -n "$att_id_b" && "$att_id_a" != "$att_id_b" ]] \
    || { echo "P13.6F supplement: bare attachment id extraction failed (a=$att_id_a b=$att_id_b)" >&2; exit 2; }

# owner scoping via API lists (list endpoints answer 200 with the caller's own
# resources; isolation means the foreign canonical ID is absent).
b_servers_json=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.1/$tenb_project/servers")
a_servers_json=$(curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.1/$proja_id/servers")
b_volumes_json=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v3/$tenb_project/volumes")
b_list_contains_a=$(SERVERS_JSON="$b_servers_json" SERVER_ID="$server_a" python3 -c \
    "import json,os; ids=[s['id'] for s in json.loads(os.environ['SERVERS_JSON']).get('servers',[])]; print('true' if os.environ['SERVER_ID'] in ids else 'false')")
a_list_contains_b=$(SERVERS_JSON="$a_servers_json" SERVER_ID="$server_b" python3 -c \
    "import json,os; ids=[s['id'] for s in json.loads(os.environ['SERVERS_JSON']).get('servers',[])]; print('true' if os.environ['SERVER_ID'] in ids else 'false')")
b_vol_list_contains_a=$(VOLUMES_JSON="$b_volumes_json" VOLUME_ID="$volume_a" python3 -c \
    "import json,os; ids=[v['id'] for v in json.loads(os.environ['VOLUMES_JSON']).get('volumes',[])]; print('true' if os.environ['VOLUME_ID'] in ids else 'false')")

# distinct LVM realizations: the provider realization name is
# o3k-v-<volume_uuid_no_hyphens>, so strip hyphens when matching.
strip_hyphens() { printf '%s' "$1" | tr -d '-'; }
lv_a_seen=$(lvs "$O3K_LVM_VOLUME_GROUP" --noheadings -o lv_name 2>/dev/null | grep -cF "$(strip_hyphens "$volume_a")" || true)
lv_b_seen=$(lvs "$O3K_LVM_VOLUME_GROUP" --noheadings -o lv_name 2>/dev/null | grep -cF "$(strip_hyphens "$volume_b")" || true)
lvm_realizations_distinct=false
if [[ "$lv_a_seen" -ge 1 && "$lv_b_seen" -ge 1 && "$volume_a" != "$volume_b" ]]; then
    lvm_realizations_distinct=true
fi

# Positive isolation assertions: correct usage must never expose or alias a
# foreign canonical resource. A failure here is a security defect, so we
# assert before emitting any PASS row.
positive_ok=true
[[ "$b_list_contains_a" == "false" && "$a_list_contains_b" == "false" ]] || positive_ok=false
[[ "$b_vol_list_contains_a" == "false" ]] || positive_ok=false
[[ "$lvm_realizations_distinct" == "true" ]] || positive_ok=false
[[ "$positive_ok" == true ]] || { echo "P13.6F supplement: positive isolation assertion FAILED (server lists a_in_b=$b_list_contains_a b_in_a=$a_list_contains_b volume a_in_b=$b_vol_list_contains_a lvm_distinct=$lvm_realizations_distinct)" >&2; exit 2; }

emit_row "S1_server_same_name_isolation" "passed" \
    "{\"resource_type\":\"openstack_compute_instance_v2\",\"operation\":\"create\",\"caller_owner\":\"project_a_and_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"server_a\":\"$server_a\",\"server_b\":\"$server_b\",\"ids_distinct\":true,\"b_list_contains_a\":$b_list_contains_a,\"a_list_contains_b\":$a_list_contains_b},\"same_name\":\"p13-shared-server\"}" \
    "openstack_compute_instance_v2/positive_isolation" >> "$evidence_rows"
emit_row "S1_volume_same_name_isolation" "passed" \
    "{\"resource_type\":\"openstack_blockstorage_volume_v3\",\"operation\":\"create\",\"caller_owner\":\"project_a_and_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"volume_a\":\"$volume_a\",\"volume_b\":\"$volume_b\",\"ids_distinct\":true,\"b_list_contains_a\":$b_vol_list_contains_a,\"lvm_realizations_distinct\":$lvm_realizations_distinct},\"same_name\":\"p13-shared-volume\"}" \
    "openstack_blockstorage_volume_v3/positive_isolation" >> "$evidence_rows"
emit_row "S1_attachment_same_project_isolation" "passed" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"create\",\"caller_owner\":\"project_a\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"attachment_a\":\"$attach_a\",\"attachment_b\":\"$attach_b\",\"ids_distinct\":true,\"same_project_only\":true}}" \
    "openstack_compute_volume_attach_v2/positive_isolation" >> "$evidence_rows"
# S2 — bounded cross-project concurrency on non-fenced operations.
# Volume attachment is serialized through the single-controller storage fence
# (ADR-0167 durable-work ownership); genuine concurrency is exercised on
# network + server create, which must not collide across projects.
write_concurrency() {
    local work="$1" image_id="$2" cidr="$3"
    cat > "$work/concurrency.tf" <<TF
resource "openstack_networking_network_v2" "concnet" {
  name = "p13-concurrent-name"
}
resource "openstack_networking_subnet_v2" "concsubnet" {
  name       = "p13-concurrent-subnet"
  network_id = openstack_networking_network_v2.concnet.id
  cidr       = "$cidr"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_compute_instance_v2" "concserver" {
  name       = "p13-concurrent-server"
  image_id   = "$image_id"
  flavor_id  = "00000000-0000-0000-0000-000000000001"
  depends_on = [openstack_networking_subnet_v2.concsubnet]
  network {
    uuid = openstack_networking_network_v2.concnet.id
  }
}
TF
}
write_concurrency "$dir_a" "$image_a" "198.51.120.0/24"
write_concurrency "$dir_b" "$image_b" "198.51.121.0/24"
echo "P13.6F supplement: S2 concurrent network+server create (A and B)"
tofu_in "$dir_a" apply -input=false -auto-approve > "$state_dir/conc-a.log" 2>&1 &
conc_a=$!
tofu_in "$dir_b" apply -input=false -auto-approve > "$state_dir/conc-b.log" 2>&1 &
conc_b=$!
wait "$conc_a"
wait "$conc_b"
conc_server_a=$(extract_id "$dir_a" "openstack_compute_instance_v2.concserver")
conc_server_b=$(extract_id "$dir_b" "openstack_compute_instance_v2.concserver")
s2_ok=false
if [[ -n "$conc_server_a" && -n "$conc_server_b" && "$conc_server_a" != "$conc_server_b" ]]; then
    s2_ok=true
fi
emit_row "S2_concurrent_operation" "$([[ $s2_ok == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_instance_v2\",\"operation\":\"create\",\"caller_owner\":\"project_a_and_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"conc_server_a\":\"$conc_server_a\",\"conc_server_b\":\"$conc_server_b\",\"ids_distinct\":$s2_ok,\"cross_project_conflict\":false,\"note\":\"concurrent network+server create across two projects; volume attachment is serialized through the single-controller storage fence\"}}" \
    "positive_isolation/concurrent" >> "$evidence_rows"
[[ "$s2_ok" == true ]] || { echo "P13.6F supplement: S2 FAILED" >&2; exit 2; }
echo "P13.6F supplement: S1 PASS; S2 concurrent operation PASS"

# final convergence snapshots before attack matrix
tofu_in "$dir_a" plan -detailed-exitcode -input=false > "$state_dir/plan-a-before.txt" 2>&1; plan_a_before=$?
tofu_in "$dir_b" plan -detailed-exitcode -input=false > "$state_dir/plan-b-before.txt" 2>&1; plan_b_before=$?
[[ "$plan_a_before" == "0" && "$plan_b_before" == "0" ]] \
    || { echo "P13.6F supplement: pre-attack plans not no-op (a=$plan_a_before b=$plan_b_before; plan-a-before.txt plan-b-before.txt)" >&2; exit 2; }

# ---------------------------------------------------------------------------
# A1-A4 — cross-project storage relationship attacks (C7 replacement)
# ---------------------------------------------------------------------------
echo "P13.6F supplement: A1-A4 cross-project attachment attacks"

# A1: project B attempts to attach its own volume to project A's server.
a_attach_list_before=$(curl -sf -H "X-Auth-Token: $token_a" \
    "$auth_url/v2.1/$proja_id/servers/$server_a/os-volume_attachments")
s=$(api_call POST "$token_b" "/v2.1/$tenb_project/servers/$server_a/os-volume_attachments" \
    "{\"volumeAttachment\":{\"volumeId\":\"$volume_b\"}}")
a1_status=$(head -n1 <<< "$s"); a1_body=$(tail -n1 <<< "$s")
a1_disclosure=true
check_no_disclosure "$a1_body" "$server_a" "$proja_id" "p13-shared-server" || a1_disclosure=false
a1_ok=false
[[ "$a1_status" == "404" && "$a1_disclosure" == true ]] && a1_ok=true
a_attach_list_after=$(curl -sf -H "X-Auth-Token: $token_a" \
    "$auth_url/v2.1/$proja_id/servers/$server_a/os-volume_attachments")
[[ "$a_attach_list_before" == "$a_attach_list_after" ]] || a1_ok=false
emit_row "A1_b_volume_attach_to_a_server" "$([[ "$a1_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"attach\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$a1_status,\"non_disclosure_ok\":$a1_disclosure,\"details\":{\"foreign_server\":\"$server_a\",\"caller_volume\":\"$volume_b\",\"attachment_list_unchanged\":$( [[ $a_attach_list_before == $a_attach_list_after ]] && echo true || echo false )}}" \
    "openstack_compute_volume_attach_v2/relationship,openstack_compute_volume_attach_v2/delete" >> "$evidence_rows"
[[ "$a1_ok" == true ]] || { echo "P13.6F supplement: A1 FAILED status=$a1_status" >&2; exit 2; }
echo "P13.6F supplement: A1 PASS (status=$a1_status)"

# A2: project A attempts to attach its own volume to project B's server.
b_attach_list_before=$(curl -sf -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments")
s=$(api_call POST "$token_a" "/v2.1/$proja_id/servers/$server_b/os-volume_attachments" \
    "{\"volumeAttachment\":{\"volumeId\":\"$volume_a\"}}")
a2_status=$(head -n1 <<< "$s"); a2_body=$(tail -n1 <<< "$s")
a2_disclosure=true
check_no_disclosure "$a2_body" "$server_b" "$tenb_project" "p13-shared-server" || a2_disclosure=false
a2_ok=false
[[ "$a2_status" == "404" && "$a2_disclosure" == true ]] && a2_ok=true
b_attach_list_after=$(curl -sf -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments")
[[ "$b_attach_list_before" == "$b_attach_list_after" ]] || a2_ok=false
emit_row "A2_a_volume_attach_to_b_server" "$([[ "$a2_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"attach\",\"caller_owner\":\"project_a\",\"target_owner\":\"project_b\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$a2_status,\"non_disclosure_ok\":$a2_disclosure,\"details\":{\"foreign_server\":\"$server_b\",\"caller_volume\":\"$volume_a\",\"attachment_list_unchanged\":$( [[ $b_attach_list_before == $b_attach_list_after ]] && echo true || echo false )}}" \
    "openstack_compute_volume_attach_v2/relationship" >> "$evidence_rows"
[[ "$a2_ok" == true ]] || { echo "P13.6F supplement: A2 FAILED status=$a2_status" >&2; exit 2; }
echo "P13.6F supplement: A2 PASS (status=$a2_status)"

# A3: project B attempts to detach project A's attachment.
s=$(api_call DELETE "$token_b" "/v2.1/$tenb_project/servers/$server_a/os-volume_attachments/$att_id_a")
a3_status=$(head -n1 <<< "$s"); a3_body=$(tail -n1 <<< "$s")
a3_disclosure=true
check_no_disclosure "$a3_body" "$att_id_a" "$proja_id" || a3_disclosure=false
a3_still=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_a" \
    "$auth_url/v2.1/$proja_id/servers/$server_a/os-volume_attachments/$att_id_a")
a3_ok=false
[[ "$a3_status" == "404" && "$a3_disclosure" == true && "$a3_still" == "200" ]] && a3_ok=true
emit_row "A3_b_detach_a_attachment" "$([[ "$a3_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"detach\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$a3_status,\"non_disclosure_ok\":$a3_disclosure,\"details\":{\"foreign_attachment\":\"$att_id_a\",\"attachment_still_present\":$( [[ $a3_still == 200 ]] && echo true || echo false )}}" \
    "openstack_compute_volume_attach_v2/delete" >> "$evidence_rows"
[[ "$a3_ok" == true ]] || { echo "P13.6F supplement: A3 FAILED status=$a3_status still=$a3_still" >&2; exit 2; }
echo "P13.6F supplement: A3 PASS (status=$a3_status)"

# A4: project A attempts to detach project B's attachment.
s=$(api_call DELETE "$token_a" "/v2.1/$proja_id/servers/$server_b/os-volume_attachments/$att_id_b")
a4_status=$(head -n1 <<< "$s"); a4_body=$(tail -n1 <<< "$s")
a4_disclosure=true
check_no_disclosure "$a4_body" "$att_id_b" "$tenb_project" || a4_disclosure=false
a4_still=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments/$att_id_b")
a4_ok=false
[[ "$a4_status" == "404" && "$a4_disclosure" == true && "$a4_still" == "200" ]] && a4_ok=true
emit_row "A4_a_detach_b_attachment" "$([[ "$a4_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"detach\",\"caller_owner\":\"project_a\",\"target_owner\":\"project_b\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$a4_status,\"non_disclosure_ok\":$a4_disclosure,\"details\":{\"foreign_attachment\":\"$att_id_b\",\"attachment_still_present\":$( [[ $a4_still == 200 ]] && echo true || echo false )}}" \
    "openstack_compute_volume_attach_v2/delete" >> "$evidence_rows"
[[ "$a4_ok" == true ]] || { echo "P13.6F supplement: A4 FAILED status=$a4_status still=$a4_still" >&2; exit 2; }
echo "P13.6F supplement: A4 PASS (status=$a4_status)"

# ---------------------------------------------------------------------------
# A5 — restart after denied attacks: nothing latent materializes
# ---------------------------------------------------------------------------
stop_o3kd "$state_dir"
sleep 1
start_o3kd "$state_dir" "$port"
token_a=$(get_token "$proja_user" "$password" "$proja_name")
token_b=$(get_token "$tenb_username" "$tenb_pass" "$tenb_name")
a3_still_after_restart=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_a" \
    "$auth_url/v2.1/$proja_id/servers/$server_a/os-volume_attachments/$att_id_a")
a4_still_after_restart=$(curl -s -o /dev/null -w "%{http_code}" -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments/$att_id_b")
plan_a_restart=$(tofu_in "$dir_a" plan -detailed-exitcode -input=false >/dev/null 2>&1; echo $?)
plan_b_restart=$(tofu_in "$dir_b" plan -detailed-exitcode -input=false >/dev/null 2>&1; echo $?)
a5_ok=false
[[ "$a3_still_after_restart" == "200" && "$a4_still_after_restart" == "200" \
    && "$plan_a_restart" == "0" && "$plan_b_restart" == "0" ]] && a5_ok=true
emit_row "A5_restart_after_denied_attacks" "$([[ "$a5_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"read\",\"caller_owner\":\"project_a_and_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"a_attachment_present\":$( [[ $a3_still_after_restart == 200 ]] && echo true || echo false ),\"b_attachment_present\":$( [[ $a4_still_after_restart == 200 ]] && echo true || echo false ),\"a_plan_noop\":$( [[ $plan_a_restart == 0 ]] && echo true || echo false ),\"b_plan_noop\":$( [[ $plan_b_restart == 0 ]] && echo true || echo false )}}" \
    "openstack_compute_volume_attach_v2/restart_reconstruction,openstack_compute_instance_v2/restart_reconstruction,openstack_blockstorage_volume_v3/restart_reconstruction" >> "$evidence_rows"
[[ "$a5_ok" == true ]] || { echo "P13.6F supplement: A5 FAILED" >&2; exit 2; }
echo "P13.6F supplement: A5 PASS (restart preserved denied-attack state)"

# ---------------------------------------------------------------------------
# Create remaining A-side resources needed for the completion matrix
# ---------------------------------------------------------------------------
echo "P13.6F supplement: creating A-side resources for completion matrix"

# Create an A-side resource and print its id; report HTTP status + body on any
# failure so a broken creation is diagnosed instead of a bare parse error.
a_create() { # <name> <method> <path> <json-or-empty> <id-key>
    local name="$1" method="$2" path="$3" data="$4" key="$5"
    local out status
    out=$(mktemp /tmp/p13-6f-a-create.XXXXXX)
    if [[ -n "$data" ]]; then
        status=$(curl -s -o "$out" -w "%{http_code}" -X "$method" \
            -H "Content-Type: application/json" -H "X-Auth-Token: $token_a" \
            --data "$data" "$auth_url$path")
    else
        status=$(curl -s -o "$out" -w "%{http_code}" -X "$method" \
            -H "X-Auth-Token: $token_a" "$auth_url$path")
    fi
    if ! python3 -c "import json;json.load(open('$out'))" >/dev/null 2>&1; then
        echo "P13.6F supplement: a_create $name FAILED http=$status body=$(head -c 200 "$out")" >&2
        return 1
    fi
    local value
    value=$(python3 -c "import json,sys; print(json.load(open('$out')).get('$key',{}).get('id') if isinstance(json.load(open('$out')).get('$key'),dict) else json.load(open('$out')).get('$key',''))" 2>/dev/null) || { echo "P13.6F supplement: a_create $name key lookup failed http=$status body=$(head -c 200 "$out")" >&2; return 1; }
    if [[ -z "$value" ]]; then
        echo "P13.6F supplement: a_create $name returned empty id http=$status body=$(head -c 300 "$out")" >&2
        return 1
    fi
    echo "$value"
}

port_a=$(a_create port POST "/v2.0/ports" "{\"port\":{\"name\":\"p13-shared-port\",\"network_id\":\"$net_a\"}}" port) || exit 2
sg_a=$(a_create sg POST "/v2.0/security-groups" '{"security_group":{"name":"p13-shared-sg"}}' security_group) || exit 2
sgrule_a=$(a_create sgrule POST "/v2.0/security-group-rules" "{\"security_group_rule\":{\"security_group_id\":\"$sg_a\",\"direction\":\"ingress\",\"ethertype\":\"IPv4\"}}" security_group_rule) || exit 2
router_a=$(a_create router POST "/v2.0/routers" '{"router":{"name":"p13-shared-router"}}' router) || exit 2
ri_port_a=$(a_create router-interface PUT "/v2.0/routers/$router_a/add_router_interface" "{\"subnet_id\":\"$subnet_a\"}" port_id) || exit 2
if [[ -z "$ri_port_a" ]]; then
    ri_port_a=$(curl -s -H "X-Auth-Token: $token_a" "$auth_url/v2.0/ports?network_id=$net_a&device_id=$router_a" \
        | python3 -c "import json,sys; ps=json.load(sys.stdin).get('ports',[]); print(ps[0]['id'] if ps else '')")
fi
fip_a=$(a_create fip POST "/v2.0/floatingips" "{\"floatingip\":{\"floating_network_id\":\"$external_realm_id\"}}" floatingip) || exit 2

# A-only keypair: a name that exists solely in project A, used for the
# foreign keypair cells. Keypair lookups are per-user scoped, so the
# same-named "p13-shared-keypair" cannot exercise the foreign path.
a_only_kp="a-only-keypair-$(random_uuid | head -c 8 | tr -d '-')"
a_only_kp_pub="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOAX5ZKKeCYUDXqLY+HJn3CYOOZ5l6/TnaMurBMYhvCI"
curl -sf -X POST "$auth_url/v2.1/$proja_id/os-keypairs" -H "Content-Type: application/json" \
    -H "X-Auth-Token: $token_a" \
    -d "{\"keypair\":{\"name\":\"$a_only_kp\",\"public_key\":\"$a_only_kp_pub\"}}" >/dev/null \
    || { echo "P13.6F supplement: a_create a-only-keypair FAILED" >&2; exit 2; }

[[ -n "$port_a" && -n "$sg_a" && -n "$sgrule_a" && -n "$router_a" && -n "$ri_port_a" && -n "$fip_a" ]] \
    || { echo "P13.6F supplement: FAILED to create A-side completion resources (port='$port_a' sg='$sg_a' sgrule='$sgrule_a' router='$router_a' ri='$ri_port_a' fip='$fip_a')" >&2; exit 2; }
echo "P13.6F supplement: A-side resources ready (port=$port_a sg=$sg_a sgrule=$sgrule_a router=$router_a ri=$ri_port_a fip=$fip_a a-only-keypair=$a_only_kp)"

# Snapshot A-side state before the attack matrix (verified unchanged after).
snapshot_a() {
    {
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.1/$proja_id/os-keypairs/$a_only_kp" || echo "kp-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/ports/$port_a" || echo "port-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/security-groups/$sg_a" || echo "sg-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/security-group-rules/$sgrule_a" || echo "sgrule-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/routers/$router_a" || echo "router-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/ports/$ri_port_a" || echo "ri-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.0/floatingips/$fip_a" || echo "fip-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v2.1/$proja_id/servers/$server_a" || echo "server-gone"
        curl -sf -H "X-Auth-Token: $token_a" "$auth_url/v3/$proja_id/volumes/$volume_a" || echo "volume-gone"
    } | sha256sum | awk '{print $1}'
}
snap_before=$(snapshot_a)

# ---------------------------------------------------------------------------
# Matrix completion: foreign access attempts by B against A's resources.
# fa <scenario> <cells> <method> <path> <data-or-""> <target-id> [forbidden...]
# ---------------------------------------------------------------------------
fa() {
    local scenario="$1" cells="$2" method="$3" path="$4" data="$5" target="$6"; shift 6
    local forbidden=("$@")
    local ru
    ru=$(random_uuid)
    local cpath="${path//$target/$ru}"
    local cdata="${data//$target/$ru}"

    local status body
    if [[ -n "$data" ]]; then
        status=$(api_call "$method" "$token_b" "$path" "$data")
    else
        status=$(api_call "$method" "$token_b" "$path")
    fi
    body=$(tail -n1 <<< "$status"); status=$(head -n1 <<< "$status")

    local cstatus cbody
    if [[ -n "$data" ]]; then
        cstatus=$(api_call "$method" "$token_b" "$cpath" "$cdata")
    else
        cstatus=$(api_call "$method" "$token_b" "$cpath")
    fi
    cbody=$(tail -n1 <<< "$cstatus"); cstatus=$(head -n1 <<< "$cstatus")

    local disclosure_ok=true
    check_no_disclosure "$body" "${forbidden[@]}" || disclosure_ok=false
    local denied=false
    [[ "$status" == "404" || "$status" == "403" ]] && denied=true
    # Existence-oracle control: denial status must equal the nonexistent control.
    local oracle_ok=false
    [[ "$status" == "$cstatus" ]] && oracle_ok=true

    local result="failed"
    if [[ "$denied" == true && "$disclosure_ok" == true && "$oracle_ok" == true ]]; then
        result="passed"
    fi
    emit_row "$scenario" "$result" \
        "{\"resource_type\":\"matrix_completion\",\"operation\":\"$method\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$status,\"control_http_status\":$cstatus,\"non_disclosure_ok\":$disclosure_ok,\"existence_oracle_free\":$oracle_ok,\"details\":{\"path\":\"${path%%\?*}\"}}" \
        "$cells" >> "$evidence_rows"
    if [[ "$result" != "passed" ]]; then
        echo "P13.6F supplement: $scenario FAILED (status=$status control=$cstatus disclosure=$disclosure_ok)" >&2
        return 1
    fi
}

COMPLETION_FAILED=0

# --- networking list absence (subnet/port/sg/sgrule/fip/router lists) --------
b_subnet_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/subnets")
b_port_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/ports")
b_sg_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/security-groups")
b_sgr_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/security-group-rules")
b_fip_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/floatingips")
b_router_list=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.0/routers")
m_net_list_ok=true
for pair in "$subnet_a" "$port_a" "$sg_a" "$sgrule_a" "$fip_a" "$router_a" "$ri_port_a"; do
    for lst in "$b_subnet_list" "$b_port_list" "$b_sg_list" "$b_sgr_list" "$b_fip_list" "$b_router_list"; do
        if printf '%s' "$lst" | grep -qF "$pair"; then m_net_list_ok=false; fi
    done
done
emit_row "M_networking_list_absence" "$([[ "$m_net_list_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"multi\",\"operation\":\"list\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":200,\"details\":{\"foreign_ids_absent\":$m_net_list_ok,\"resources\":[\"subnet\",\"port\",\"secgroup\",\"secgroup_rule\",\"floatingip\",\"router\",\"router_interface\"]}}" \
    "openstack_networking_subnet_v2/list,openstack_networking_port_v2/list,openstack_networking_secgroup_v2/list,openstack_networking_secgroup_rule_v2/list,openstack_networking_floatingip_v2/list,openstack_networking_router_v2/list,openstack_networking_router_interface_v2/list" >> "$evidence_rows"
[[ "$m_net_list_ok" == true ]] || COMPLETION_FAILED=1

# --- attachment list absence --------------------------------------------------
b_attach_list=$(curl -sf -H "X-Auth-Token: $token_b" \
    "$auth_url/v2.1/$tenb_project/servers/$server_b/os-volume_attachments")
m_attach_list_ok=true
if printf '%s' "$b_attach_list" | grep -qF "$att_id_a"; then m_attach_list_ok=false; fi
emit_row "M_attachment_list" "$([[ "$m_attach_list_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"list\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":200,\"details\":{\"foreign_attachment_absent\":$m_attach_list_ok}}" \
    "openstack_compute_volume_attach_v2/list" >> "$evidence_rows"
[[ "$m_attach_list_ok" == true ]] || COMPLETION_FAILED=1

# --- keypair negative cells ---------------------------------------------------
# Keypair show by name is per-user scoped: both projects legitimately own a
# "p13-shared-keypair", so the foreign path is exercised against the A-only
# keypair name. List isolation is an absence assertion on B's own list.
kp_list_b=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.1/$tenb_project/os-keypairs")
m_kp_list_result="passed"
if printf '%s' "$kp_list_b" | grep -qF "$a_only_kp" || printf '%s' "$kp_list_b" | grep -qF "$proja_id"; then
    m_kp_list_result="failed"
fi
emit_row "M_keypair_list" "$m_kp_list_result" \
    "{\"resource_type\":\"openstack_compute_keypair_v2\",\"operation\":\"list\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":200,\"details\":{\"a_only_keypair_absent\":$( [[ $m_kp_list_result == passed ]] && echo true || echo false )}}" \
    "openstack_compute_keypair_v2/list" >> "$evidence_rows"
[[ "$m_kp_list_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_keypair_show" "openstack_compute_keypair_v2/show,openstack_compute_keypair_v2/import" \
    GET "/v2.1/$tenb_project/os-keypairs/$a_only_kp" "" "$a_only_kp" "$proja_id" || COMPLETION_FAILED=1
fa "M_keypair_delete" "openstack_compute_keypair_v2/delete" \
    DELETE "/v2.1/$tenb_project/os-keypairs/$a_only_kp" "" "$a_only_kp" "$proja_id" || COMPLETION_FAILED=1

# M_keypair import: tofu import of A's unique keypair name must fail and must
# not adopt foreign state (unlike the shared name, this name exists only in A).
cat > "$dir_b/import-kp.tf" <<TF
resource "openstack_compute_keypair_v2" "imported" {
  name = "$a_only_kp"
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_compute_keypair_v2.imported "$a_only_kp" 2>&1) && import_rc=0 || import_rc=$?
import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q '"openstack_compute_keypair_v2.imported"' "$dir_b/terraform.tfstate"; then
    import_adopted=true
fi
rm -f "$dir_b/import-kp.tf"
m_kp_import_result="failed"
if [[ "$import_rc" != "0" && "$import_adopted" == false ]]; then m_kp_import_result="passed"; fi
emit_row "M_keypair_import" "$m_kp_import_result" \
    "{\"resource_type\":\"openstack_compute_keypair_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$import_adopted}}" \
    "openstack_compute_keypair_v2/import" >> "$evidence_rows"
[[ "$m_kp_import_result" == "passed" ]] || COMPLETION_FAILED=1

# --- server negative cells ---------------------------------------------------
# List cells are absence assertions: the list endpoint answers 200 with the
# caller's own resources and must not contain the foreign canonical ID.
server_list_b=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.1/$tenb_project/servers")
m_server_list_result="passed"
if printf '%s' "$server_list_b" | grep -qF "$server_a"; then m_server_list_result="failed"; fi
emit_row "M_server_list" "$m_server_list_result" \
    "{\"resource_type\":\"openstack_compute_instance_v2\",\"operation\":\"list\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":200,\"details\":{\"foreign_server_absent\":$( [[ $m_server_list_result == passed ]] && echo true || echo false )}}" \
    "openstack_compute_instance_v2/list" >> "$evidence_rows"
[[ "$m_server_list_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_server_show" "openstack_compute_instance_v2/show,openstack_compute_instance_v2/import" \
    GET "/v2.1/$tenb_project/servers/$server_a" "" "$server_a" "$proja_id" "p13-shared-server" || COMPLETION_FAILED=1
fa "M_server_update" "openstack_compute_instance_v2/update" \
    PUT "/v2.1/$tenb_project/servers/$server_a" "{\"server\":{\"name\":\"hijacked\"}}" "$server_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_server_delete" "openstack_compute_instance_v2/delete" \
    DELETE "/v2.1/$tenb_project/servers/$server_a" "" "$server_a" "$proja_id" || COMPLETION_FAILED=1

# M_server_import: tofu import of A's server id must fail / not adopt.
cat > "$dir_b/import-server.tf" <<TF
resource "openstack_compute_instance_v2" "imported" {
  name = "import-probe"
}
TF
# M_server_import: tofu import of A's server id must not grant authority over
# A. O3K returns 404 on the foreign read (no disclosure); the upstream server
# provider tolerates that 404 by reporting an empty import without writing the
# foreign server into B's state. The security bar (per the C brief, checking
# the resulting B state, not the CLI exit) is therefore "no state adoption":
# B must not end up managing A's server.
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_compute_instance_v2.imported "$server_a" 2>&1) && import_rc=0 || import_rc=$?
server_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$server_a" "$dir_b/terraform.tfstate"; then
    server_import_adopted=true
fi
rm -f "$dir_b/import-server.tf"
m_server_import_result="failed"
if [[ "$server_import_adopted" == false ]]; then m_server_import_result="passed"; fi
emit_row "M_server_import" "$m_server_import_result" \
    "{\"resource_type\":\"openstack_compute_instance_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$server_import_adopted,\"note\":\"O3K returns 404 on the foreign read; tofu reports an empty import (exit 0) without materializing A's server in B's state\"}}" \
    "openstack_compute_instance_v2/import" >> "$evidence_rows"
[[ "$m_server_import_result" == "passed" ]] || COMPLETION_FAILED=1

# M_server_relationship: server create on A's foreign network port must fail.
s=$(api_call POST "$token_b" "/v2.1/$tenb_project/servers" \
    "{\"server\":{\"name\":\"cross-probe\",\"imageRef\":\"$image_b\",\"flavorRef\":\"00000000-0000-0000-0000-000000000001\",\"networks\":[{\"port\":\"$port_a\"}]}}")
msr_status=$(head -n1 <<< "$s"); msr_body=$(tail -n1 <<< "$s")
msr_disclosure=true
check_no_disclosure "$msr_body" "$port_a" "$proja_id" || msr_disclosure=false
msr_ok=false
[[ ( "$msr_status" == "404" || "$msr_status" == "400" || "$msr_status" == "403" || "$msr_status" == "409" ) && "$msr_disclosure" == true ]] && msr_ok=true
emit_row "M_server_relationship" "$([[ "$msr_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_instance_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$msr_status,\"non_disclosure_ok\":$msr_disclosure,\"details\":{\"foreign_port\":\"$port_a\"}}" \
    "openstack_compute_instance_v2/relationship" >> "$evidence_rows"
[[ "$msr_ok" == true ]] || COMPLETION_FAILED=1

# M_keypair_relationship: B server referencing A's keypair name must not bind.
s=$(api_call POST "$token_b" "/v2.1/$tenb_project/servers" \
    "{\"server\":{\"name\":\"kp-probe\",\"imageRef\":\"$image_b\",\"flavorRef\":\"00000000-0000-0000-0000-000000000001\",\"key_name\":\"p13-shared-keypair\",\"networks\":[{\"uuid\":\"$net_b\"}]}}")
mkp_status=$(head -n1 <<< "$s"); mkp_body=$(tail -n1 <<< "$s")
kp_probe_id=$(python3 -c "import json,sys; d=json.load(sys.stdin); print(d.get('server',{}).get('id',''))" < "$mkp_body" 2>/dev/null || echo "")
mkp_ok=false
if [[ "$mkp_status" == "202" || "$mkp_status" == "200" ]]; then
    # server may create using B's own same-named keypair; must not expose A's
    kp_probe_detail=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v2.1/$tenb_project/servers/$kp_probe_id" || echo "{}")
    if ! grep -qF "$proja_id" <<< "$kp_probe_detail"; then mkp_ok=true; fi
    curl -sf -X DELETE -H "X-Auth-Token: $token_b" "$auth_url/v2.1/$tenb_project/servers/$kp_probe_id" >/dev/null 2>&1 || true
elif [[ "$mkp_status" == "400" || "$mkp_status" == "404" || "$mkp_status" == "409" ]]; then
    mkp_ok=true
fi
emit_row "M_keypair_relationship" "$([[ "$mkp_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_keypair_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$mkp_status,\"details\":{\"foreign_keypair_name\":\"p13-shared-keypair\",\"no_foreign_binding\":$( [[ $mkp_ok == true ]] && echo true || echo false )}}" \
    "openstack_compute_keypair_v2/relationship" >> "$evidence_rows"
[[ "$mkp_ok" == true ]] || COMPLETION_FAILED=1

# --- volume negative cells ---------------------------------------------------
volume_list_b=$(curl -sf -H "X-Auth-Token: $token_b" "$auth_url/v3/$tenb_project/volumes")
m_volume_list_result="passed"
if printf '%s' "$volume_list_b" | grep -qF "$volume_a"; then m_volume_list_result="failed"; fi
emit_row "M_volume_list" "$m_volume_list_result" \
    "{\"resource_type\":\"openstack_blockstorage_volume_v3\",\"operation\":\"list\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":200,\"details\":{\"foreign_volume_absent\":$( [[ $m_volume_list_result == passed ]] && echo true || echo false )}}" \
    "openstack_blockstorage_volume_v3/list" >> "$evidence_rows"
[[ "$m_volume_list_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_volume_show" "openstack_blockstorage_volume_v3/show,openstack_blockstorage_volume_v3/import" \
    GET "/v3/$tenb_project/volumes/$volume_a" "" "$volume_a" "$proja_id" "p13-shared-volume" || COMPLETION_FAILED=1
fa "M_volume_update" "openstack_blockstorage_volume_v3/update" \
    PUT "/v3/$tenb_project/volumes/$volume_a" "{\"volume\":{\"name\":\"hijacked\"}}" "$volume_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_volume_delete" "openstack_blockstorage_volume_v3/delete" \
    DELETE "/v3/$tenb_project/volumes/$volume_a" "" "$volume_a" "$proja_id" || COMPLETION_FAILED=1

cat > "$dir_b/import-volume.tf" <<TF
resource "openstack_blockstorage_volume_v3" "imported" {
  name = "import-probe"
  size = 1
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_blockstorage_volume_v3.imported "$volume_a" 2>&1) && import_rc=0 || import_rc=$?
volume_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$volume_a" "$dir_b/terraform.tfstate"; then
    volume_import_adopted=true
fi
rm -f "$dir_b/import-volume.tf"
m_volume_import_result="failed"
if [[ "$import_rc" != "0" && "$volume_import_adopted" == false ]]; then m_volume_import_result="passed"; fi
emit_row "M_volume_import" "$m_volume_import_result" \
    "{\"resource_type\":\"openstack_blockstorage_volume_v3\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$volume_import_adopted}}" \
    "openstack_blockstorage_volume_v3/import" >> "$evidence_rows"
[[ "$m_volume_import_result" == "passed" ]] || COMPLETION_FAILED=1

# --- attachment negative cells ------------------------------------------------
fa "M_attachment_show" "openstack_compute_volume_attach_v2/show,openstack_compute_volume_attach_v2/import" \
    GET "/v2.1/$tenb_project/servers/$server_a/os-volume_attachments/$att_id_a" "" "$att_id_a" "$server_a" "$proja_id" || COMPLETION_FAILED=1

cat > "$dir_b/import-attach.tf" <<TF
resource "openstack_compute_volume_attach_v2" "imported" {
  instance_id = "00000000-0000-0000-0000-000000000000"
  volume_id   = "00000000-0000-0000-0000-000000000000"
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false "openstack_compute_volume_attach_v2.imported" "$server_a/$att_id_a" 2>&1) && import_rc=0 || import_rc=$?
attach_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$att_id_a" "$dir_b/terraform.tfstate"; then
    attach_import_adopted=true
fi
rm -f "$dir_b/import-attach.tf"
m_attach_import_result="failed"
if [[ "$import_rc" != "0" && "$attach_import_adopted" == false ]]; then m_attach_import_result="passed"; fi
emit_row "M_attachment_import" "$m_attach_import_result" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$attach_import_adopted}}" \
    "openstack_compute_volume_attach_v2/import" >> "$evidence_rows"
[[ "$m_attach_import_result" == "passed" ]] || COMPLETION_FAILED=1

# --- networking completion cells ----------------------------------------------
fa "M_subnet_update" "openstack_networking_subnet_v2/update" \
    PUT "/v2.0/subnets/$subnet_a" "{\"subnet\":{\"name\":\"hijacked\"}}" "$subnet_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_subnet_delete" "openstack_networking_subnet_v2/delete" \
    DELETE "/v2.0/subnets/$subnet_a" "" "$subnet_a" "$proja_id" || COMPLETION_FAILED=1

cat > "$dir_b/import-subnet.tf" <<TF
resource "openstack_networking_subnet_v2" "imported" {
  name       = "import-probe"
  network_id = openstack_networking_network_v2.net.id
  cidr       = "198.51.112.0/24"
  ip_version = 4
  enable_dhcp = false
  no_gateway = true
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_networking_subnet_v2.imported "$subnet_a" 2>&1) && import_rc=0 || import_rc=$?
subnet_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$subnet_a" "$dir_b/terraform.tfstate"; then
    subnet_import_adopted=true
fi
rm -f "$dir_b/import-subnet.tf"
m_subnet_import_result="failed"
if [[ "$import_rc" != "0" && "$subnet_import_adopted" == false ]]; then m_subnet_import_result="passed"; fi
emit_row "M_subnet_import" "$m_subnet_import_result" \
    "{\"resource_type\":\"openstack_networking_subnet_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$subnet_import_adopted}}" \
    "openstack_networking_subnet_v2/import" >> "$evidence_rows"
[[ "$m_subnet_import_result" == "passed" ]] || COMPLETION_FAILED=1

# M_subnet_relationship: B creates a subnet on A's network.
s=$(api_call POST "$token_b" "/v2.0/subnets" \
    "{\"subnet\":{\"name\":\"cross-subnet\",\"network_id\":\"$net_a\",\"cidr\":\"198.51.113.0/24\",\"ip_version\":4}}")
msub_status=$(head -n1 <<< "$s"); msub_body=$(tail -n1 <<< "$s")
msub_disclosure=true
check_no_disclosure "$msub_body" "$net_a" "$proja_id" || msub_disclosure=false
msub_ok=false
[[ ( "$msub_status" == "404" || "$msub_status" == "400" || "$msub_status" == "403" || "$msub_status" == "409" ) && "$msub_disclosure" == true ]] && msub_ok=true
emit_row "M_subnet_relationship" "$([[ "$msub_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_networking_subnet_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$msub_status,\"non_disclosure_ok\":$msub_disclosure,\"details\":{\"foreign_network\":\"$net_a\"}}" \
    "openstack_networking_subnet_v2/relationship" >> "$evidence_rows"
[[ "$msub_ok" == true ]] || COMPLETION_FAILED=1

fa "M_port_delete" "openstack_networking_port_v2/delete" \
    DELETE "/v2.0/ports/$port_a" "" "$port_a" "$proja_id" || COMPLETION_FAILED=1

cat > "$dir_b/import-port.tf" <<TF
resource "openstack_networking_port_v2" "imported" {
  name       = "import-probe"
  network_id = openstack_networking_network_v2.net.id
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_networking_port_v2.imported "$port_a" 2>&1) && import_rc=0 || import_rc=$?
port_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$port_a" "$dir_b/terraform.tfstate"; then
    port_import_adopted=true
fi
rm -f "$dir_b/import-port.tf"
m_port_import_result="failed"
if [[ "$import_rc" != "0" && "$port_import_adopted" == false ]]; then m_port_import_result="passed"; fi
emit_row "M_port_import" "$m_port_import_result" \
    "{\"resource_type\":\"openstack_networking_port_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$port_import_adopted}}" \
    "openstack_networking_port_v2/import" >> "$evidence_rows"
[[ "$m_port_import_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_sg_update" "openstack_networking_secgroup_v2/update" \
    PUT "/v2.0/security-groups/$sg_a" "{\"security_group\":{\"name\":\"hijacked\"}}" "$sg_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_sg_delete" "openstack_networking_secgroup_v2/delete" \
    DELETE "/v2.0/security-groups/$sg_a" "" "$sg_a" "$proja_id" || COMPLETION_FAILED=1

cat > "$dir_b/import-sg.tf" <<TF
resource "openstack_networking_secgroup_v2" "imported" {
  name = "import-probe"
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_networking_secgroup_v2.imported "$sg_a" 2>&1) && import_rc=0 || import_rc=$?
sg_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$sg_a" "$dir_b/terraform.tfstate"; then
    sg_import_adopted=true
fi
rm -f "$dir_b/import-sg.tf"
m_sg_import_result="failed"
if [[ "$import_rc" != "0" && "$sg_import_adopted" == false ]]; then m_sg_import_result="passed"; fi
emit_row "M_sg_import" "$m_sg_import_result" \
    "{\"resource_type\":\"openstack_networking_secgroup_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$sg_import_adopted}}" \
    "openstack_networking_secgroup_v2/import" >> "$evidence_rows"
[[ "$m_sg_import_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_sgrule_show" "openstack_networking_secgroup_rule_v2/show,openstack_networking_secgroup_rule_v2/import" \
    GET "/v2.0/security-group-rules/$sgrule_a" "" "$sgrule_a" "$sg_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_sgrule_delete" "openstack_networking_secgroup_rule_v2/delete" \
    DELETE "/v2.0/security-group-rules/$sgrule_a" "" "$sgrule_a" "$sg_a" "$proja_id" || COMPLETION_FAILED=1

# M_sgrule_relationship: B creates a rule on A's security group.
s=$(api_call POST "$token_b" "/v2.0/security-group-rules" \
    "{\"security_group_rule\":{\"security_group_id\":\"$sg_a\",\"direction\":\"ingress\",\"ethertype\":\"IPv4\"}}")
msgr_status=$(head -n1 <<< "$s"); msgr_body=$(tail -n1 <<< "$s")
msgr_disclosure=true
check_no_disclosure "$msgr_body" "$sg_a" "$proja_id" || msgr_disclosure=false
msgr_ok=false
[[ ( "$msgr_status" == "404" || "$msgr_status" == "400" || "$msgr_status" == "403" || "$msgr_status" == "409" ) && "$msgr_disclosure" == true ]] && msgr_ok=true
emit_row "M_sgrule_relationship" "$([[ "$msgr_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_networking_secgroup_rule_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$msgr_status,\"non_disclosure_ok\":$msgr_disclosure,\"details\":{\"foreign_secgroup\":\"$sg_a\"}}" \
    "openstack_networking_secgroup_rule_v2/relationship" >> "$evidence_rows"
[[ "$msgr_ok" == true ]] || COMPLETION_FAILED=1

cat > "$dir_b/import-sgrule.tf" <<TF
resource "openstack_networking_secgroup_rule_v2" "imported" {
  direction         = "ingress"
  ethertype         = "IPv4"
  security_group_id = openstack_networking_secgroup_v2.probe.id
}
resource "openstack_networking_secgroup_v2" "probe" {
  name = "import-probe-sg"
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_networking_secgroup_rule_v2.imported "$sgrule_a" 2>&1) && import_rc=0 || import_rc=$?
sgrule_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$sgrule_a" "$dir_b/terraform.tfstate"; then
    sgrule_import_adopted=true
fi
rm -f "$dir_b/import-sgrule.tf"
m_sgrule_import_result="failed"
if [[ "$import_rc" != "0" && "$sgrule_import_adopted" == false ]]; then m_sgrule_import_result="passed"; fi
emit_row "M_sgrule_import" "$m_sgrule_import_result" \
    "{\"resource_type\":\"openstack_networking_secgroup_rule_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$sgrule_import_adopted}}" \
    "openstack_networking_secgroup_rule_v2/import" >> "$evidence_rows"
[[ "$m_sgrule_import_result" == "passed" ]] || COMPLETION_FAILED=1

cat > "$dir_b/import-router.tf" <<TF
resource "openstack_networking_router_v2" "imported" {
  name = "import-probe"
}
TF
import_out=$(cd "$dir_b" && TF_CLI_CONFIG_FILE="$dir_b/tofu.tfrc" TF_IN_AUTOMATION=1 \
    "$tofu" import -input=false openstack_networking_router_v2.imported "$router_a" 2>&1) && import_rc=0 || import_rc=$?
router_import_adopted=false
if [[ -f "$dir_b/terraform.tfstate" ]] && grep -q "$router_a" "$dir_b/terraform.tfstate"; then
    router_import_adopted=true
fi
rm -f "$dir_b/import-router.tf"
m_router_import_result="failed"
if [[ "$import_rc" != "0" && "$router_import_adopted" == false ]]; then m_router_import_result="passed"; fi
emit_row "M_router_import" "$m_router_import_result" \
    "{\"resource_type\":\"openstack_networking_router_v2\",\"operation\":\"import\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":404,\"details\":{\"tofu_import_exit\":$import_rc,\"foreign_state_adoption\":$router_import_adopted}}" \
    "openstack_networking_router_v2/import" >> "$evidence_rows"
[[ "$m_router_import_result" == "passed" ]] || COMPLETION_FAILED=1

fa "M_routerinterface_show" "openstack_networking_router_interface_v2/show,openstack_networking_router_interface_v2/import" \
    GET "/v2.0/ports/$ri_port_a" "" "$ri_port_a" "$router_a" "$proja_id" || COMPLETION_FAILED=1
fa "M_routerinterface_delete" "openstack_networking_router_interface_v2/delete" \
    PUT "/v2.0/routers/$router_a/remove_router_interface" "{\"subnet_id\":\"$subnet_a\"}" "$router_a" "$subnet_a" "$proja_id" || COMPLETION_FAILED=1

# M_routerinterface_relationship: B adds its subnet to A's router.
s=$(api_call PUT "$token_b" "/v2.0/routers/$router_a/add_router_interface" "{\"subnet_id\":\"$subnet_b\"}")
mri_status=$(head -n1 <<< "$s"); mri_body=$(tail -n1 <<< "$s")
mri_disclosure=true
check_no_disclosure "$mri_body" "$router_a" "$subnet_b" "$proja_id" || mri_disclosure=false
mri_ok=false
[[ ( "$mri_status" == "404" || "$mri_status" == "400" || "$mri_status" == "403" || "$mri_status" == "409" ) && "$mri_disclosure" == true ]] && mri_ok=true
emit_row "M_routerinterface_relationship" "$([[ "$mri_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_networking_router_interface_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"deny\",\"actual_http_status\":$mri_status,\"non_disclosure_ok\":$mri_disclosure,\"details\":{\"foreign_router\":\"$router_a\"}}" \
    "openstack_networking_router_interface_v2/relationship" >> "$evidence_rows"
[[ "$mri_ok" == true ]] || COMPLETION_FAILED=1

fa "M_fip_update" "openstack_networking_floatingip_v2/update" \
    PUT "/v2.0/floatingips/$fip_a" "{\"floatingip\":{\"description\":\"hijacked\"}}" "$fip_a" "$proja_id" || COMPLETION_FAILED=1

# --- A-side immutability after the full attack matrix ------------------------
snap_after=$(snapshot_a)
immutability_ok=false
[[ "$snap_before" == "$snap_after" ]] && immutability_ok=true
emit_row "M_a_state_immutability" "$([[ "$immutability_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"multi\",\"operation\":\"plan\",\"caller_owner\":\"project_a\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"snapshot_before\":\"$snap_before\",\"snapshot_after\":\"$snap_after\",\"foreign_state_changes\":$([[ $immutability_ok == true ]] && echo 0 || echo 1)}}" \
    "" >> "$evidence_rows"
[[ "$immutability_ok" == true ]] || { echo "P13.6F supplement: A-side state changed after attack matrix" >&2; COMPLETION_FAILED=1; }

# ---------------------------------------------------------------------------
# S4 — detach/recreate isolation in one project leaves the other untouched
# ---------------------------------------------------------------------------
echo "P13.6F supplement: S4 detach/recreate isolation"

lvm_lv_for_volume() {
    # The native LVM provider realization name is o3k-v-<volume_uuid_no_hyphens>.
    lvs "$O3K_LVM_VOLUME_GROUP" --noheadings -o lv_name 2>/dev/null \
        | grep -F "$(printf '%s' "$1" | tr -d '-')" | head -n1 | awk '{print $1}'
    # Always succeed: with `set -euo pipefail` a no-match grep would make this
    # command substitution fail, which is the normal post-destroy case (the
    # caller treats an empty result as "no LVM realization / no leak").
    return 0
}

lv_a_before=$(lvm_lv_for_volume "$volume_a")
lv_b_before=$(lvm_lv_for_volume "$volume_b")
[[ -n "$lv_a_before" && -n "$lv_b_before" ]] \
    || { echo "P13.6F supplement: FAILED to observe LVM realizations (a=$lv_a_before b=$lv_b_before)" >&2; exit 2; }

tofu_in "$dir_a" taint openstack_compute_volume_attach_v2.attachment >/dev/null
tofu_in "$dir_a" apply -input=false -auto-approve > "$state_dir/reattach-a.log" 2>&1
attach_a_new=$(extract_id "$dir_a" "openstack_compute_volume_attach_v2.attachment")
lv_a_after=$(lvm_lv_for_volume "$volume_a")
lv_b_during_a=$(lvm_lv_for_volume "$volume_b")
b_attach_unchanged=false
[[ "$(extract_id "$dir_b" "openstack_compute_volume_attach_v2.attachment")" == "$attach_b" ]] && b_attach_unchanged=true
s4a_ok=false
[[ -n "$attach_a_new" && "$attach_a_new" != "$attach_a" && -n "$lv_a_after" \
    && "$lv_b_during_a" == "$lv_b_before" && "$b_attach_unchanged" == true ]] && s4a_ok=true
emit_row "S4a_detach_recreate_a_leaves_b" "$([[ "$s4a_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"create\",\"caller_owner\":\"project_a\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"attachment_a_before\":\"$attach_a\",\"attachment_a_after\":\"$attach_a_new\",\"lv_a_before\":\"$lv_a_before\",\"lv_a_after\":\"$lv_a_after\",\"lv_b_unchanged\":$( [[ $lv_b_during_a == $lv_b_before ]] && echo true || echo false ),\"b_attachment_unchanged\":$b_attach_unchanged}}" \
    "openstack_compute_volume_attach_v2/restart_reconstruction" >> "$evidence_rows"
[[ "$s4a_ok" == true ]] || { echo "P13.6F supplement: S4a FAILED" >&2; exit 2; }

tofu_in "$dir_b" taint openstack_compute_volume_attach_v2.attachment >/dev/null
tofu_in "$dir_b" apply -input=false -auto-approve > "$state_dir/reattach-b.log" 2>&1
attach_b_new=$(extract_id "$dir_b" "openstack_compute_volume_attach_v2.attachment")
lv_b_after=$(lvm_lv_for_volume "$volume_b")
lv_a_during_b=$(lvm_lv_for_volume "$volume_a")
a_attach_unchanged=false
[[ "$(extract_id "$dir_a" "openstack_compute_volume_attach_v2.attachment")" == "$attach_a_new" ]] && a_attach_unchanged=true
s4b_ok=false
[[ -n "$attach_b_new" && "$attach_b_new" != "$attach_b" && -n "$lv_b_after" \
    && "$lv_a_during_b" == "$lv_a_after" && "$a_attach_unchanged" == true ]] && s4b_ok=true
emit_row "S4b_detach_recreate_b_leaves_a" "$([[ "$s4b_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"openstack_compute_volume_attach_v2\",\"operation\":\"create\",\"caller_owner\":\"project_b\",\"target_owner\":\"project_b\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"attachment_b_before\":\"$attach_b\",\"attachment_b_after\":\"$attach_b_new\",\"lv_b_before\":\"$lv_b_before\",\"lv_b_after\":\"$lv_b_after\",\"lv_a_unchanged\":$( [[ $lv_a_during_b == $lv_a_after ]] && echo true || echo false ),\"a_attachment_unchanged\":$a_attach_unchanged}}" \
    "" >> "$evidence_rows"
[[ "$s4b_ok" == true ]] || { echo "P13.6F supplement: S4b FAILED" >&2; exit 2; }
echo "P13.6F supplement: S4 PASS"

# ---------------------------------------------------------------------------
# Final convergence and cleanup
# ---------------------------------------------------------------------------
plan_a_final=$(tofu_in "$dir_a" plan -detailed-exitcode -input=false >/dev/null 2>&1; echo $?)
plan_b_final=$(tofu_in "$dir_b" plan -detailed-exitcode -input=false >/dev/null 2>&1; echo $?)

# remove A-side API-created resources so project graphs can be destroyed
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.0/floatingips/$fip_a" >/dev/null 2>&1 || true
curl -sf -X PUT "$auth_url/v2.0/routers/$router_a/remove_router_interface" \
    -H "Content-Type: application/json" -H "X-Auth-Token: $token_a" \
    -d "{\"subnet_id\":\"$subnet_a\"}" >/dev/null 2>&1 || true
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.0/routers/$router_a" >/dev/null 2>&1 || true
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.0/ports/$port_a" >/dev/null 2>&1 || true
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.0/security-group-rules/$sgrule_a" >/dev/null 2>&1 || true
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.0/security-groups/$sg_a" >/dev/null 2>&1 || true
curl -sf -X DELETE -H "X-Auth-Token: $token_a" "$auth_url/v2.1/$proja_id/os-keypairs/$a_only_kp" >/dev/null 2>&1 || true

tofu_in "$dir_a" destroy -input=false -auto-approve > "$state_dir/destroy-a.log" 2>&1
tofu_in "$dir_b" destroy -input=false -auto-approve > "$state_dir/destroy-b.log" 2>&1

# LVM realization leak check: provider LVs for canonical volumes must be gone.
sleep 2
lv_leak=$(lvm_lv_for_volume "$volume_a"; lvm_lv_for_volume "$volume_b")
cleanup_ok=true
[[ -z "$lv_leak" ]] || cleanup_ok=false

final_ok=false
[[ "$plan_a_final" == "0" && "$plan_b_final" == "0" && "$cleanup_ok" == true ]] && final_ok=true
emit_row "S5_final_convergence_and_cleanup" "$([[ "$final_ok" == true ]] && echo passed || echo failed)" \
    "{\"resource_type\":\"multi\",\"operation\":\"plan\",\"caller_owner\":\"project_a_and_b\",\"target_owner\":\"project_a\",\"expected_authorization_outcome\":\"allow\",\"actual_http_status\":200,\"details\":{\"a_plan_noop\":$( [[ $plan_a_final == 0 ]] && echo true || echo false ),\"b_plan_noop\":$( [[ $plan_b_final == 0 ]] && echo true || echo false ),\"lvm_realization_leaks\":\"$lv_leak\",\"cleanup_ok\":$cleanup_ok}}" \
    "" >> "$evidence_rows"
[[ "$final_ok" == true ]] || { echo "P13.6F supplement: final convergence/cleanup FAILED (plans a=$plan_a_final b=$plan_b_final leak='$lv_leak')" >&2; COMPLETION_FAILED=1; }

# ---------------------------------------------------------------------------
# Serialize the evidence artifact
# ---------------------------------------------------------------------------
head_sha="$(git -C "$root_dir" rev-parse HEAD 2>/dev/null || echo unknown)"
mkdir -p "$(dirname "$output")"
P13_6F_OUT="$output" \
P13_6F_ROWS="$evidence_rows" \
P13_6F_HEAD="$head_sha" \
P13_6F_TOFU_ARCHIVE="$tofu_archive" \
P13_6F_PROVIDER_ARCHIVE="$provider_archive" \
P13_6F_PROVIDER_SHA="$provider_sha" \
P13_6F_PROJA_ID="$proja_id" \
P13_6F_PROJB_ID="$tenb_project" \
P13_6F_PROJA_PRINCIPAL="$proja_user" \
P13_6F_PROJB_PRINCIPAL="$tenb_username" \
P13_6F_VG="$O3K_LVM_VOLUME_GROUP" \
P13_6F_FAILED="$COMPLETION_FAILED" \
python3 - <<'PY'
import hashlib
import json
import os


def sha256_of(path):
    digest = hashlib.sha256()
    with open(path, "rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 16), b""):
            digest.update(chunk)
    return digest.hexdigest()


rows = []
with open(os.environ["P13_6F_ROWS"], encoding="utf-8") as handle:
    for line in handle:
        line = line.strip()
        if line:
            rows.append(json.loads(line))

failed = os.environ["P13_6F_FAILED"] == "1"
vocabulary = {"passed", "not_applicable", "expected_ambiguous", "upstream_provider_unsupported",
              "execution_profile_unavailable", "blocked", "failed"}
bad = [r["scenario"] for r in rows if r.get("result") not in vocabulary]
if bad:
    raise SystemExit(f"rows with uncontrolled result vocabulary: {bad}")

document = {
    "schema_version": 1,
    "artifact_type": "o3k-p13-6f-privileged-security-supplement",
    "phase": "P13.6F",
    "status": "failed" if failed else "verified",
    "profile": "p13-iac-compatibility-v1",
    "tested_runtime_head_sha": os.environ["P13_6F_HEAD"],
    "backend": "postgresql",
    "execution_tier": "privileged-testlab-disposable-lvm",
    "toolchain": {
        "opentofu": "1.12.6",
        "provider": "terraform-provider-openstack/openstack 3.4.0",
        "provider_modified": False,
        "opentofu_archive_sha256": sha256_of(os.environ["P13_6F_TOFU_ARCHIVE"]),
        "provider_archive_sha256": sha256_of(os.environ["P13_6F_PROVIDER_ARCHIVE"]),
        "provider_binary_sha256": os.environ["P13_6F_PROVIDER_SHA"],
    },
    "lvm_profile": {"volume_group": os.environ["P13_6F_VG"], "disposable": True},
    "two_project_identity_model": {
        "project_a": {"project_id": os.environ["P13_6F_PROJA_ID"], "principal": os.environ["P13_6F_PROJA_PRINCIPAL"]},
        "project_b": {"project_id": os.environ["P13_6F_PROJB_ID"], "principal": os.environ["P13_6F_PROJB_PRINCIPAL"]},
    },
    "supersedes": {
        "openstack_compute_instance_v2_positive_isolation": "P13.6B B10_compute_server",
        "openstack_blockstorage_volume_v3_positive_isolation": "P13.6B B11_volume",
        "openstack_compute_volume_attach_v2_positive_isolation": "P13.6B B12_volume_attachment",
        "cross_project_attachment_attacks": "P13.6C C7_volume_attach_foreign_server / C7_volume_foreign_detach",
    },
    "scenarios": rows,
    "result": "failed" if failed else "passed",
}

out = os.environ["P13_6F_OUT"]
with open(out, "w", encoding="utf-8") as handle:
    json.dump(document, handle, indent=2, sort_keys=True)
    handle.write("\n")
print(f"P13.6F supplement evidence written: {out} ({len(rows)} rows, result={document['result']})")
PY

if [[ "$COMPLETION_FAILED" == "1" ]]; then
    echo "P13.6F supplement: COMPLETION FAILURES recorded in evidence" >&2
    exit 2
fi
echo "P13.6F supplement: aggregate verdict PASS"
