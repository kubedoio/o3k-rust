#!/usr/bin/env bash
set -euo pipefail

# Portable real-provider smoke gate.  CI supplies the pinned toolchain and a
# locally built o3kd; no OpenStack cloud or provider fork is used.
: "${O3K_P13_TOFU:?run scripts/p13_prepare_toolchain.sh first}"
: "${O3K_P13_PROVIDER_BINARY:?run scripts/p13_prepare_toolchain.sh first}"
root_dir=$(cd "$(dirname "$0")/.." && pwd)
o3kd=${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}
[[ -x "$o3kd" ]] || { echo "missing o3kd: $o3kd" >&2; exit 2; }
work=$(mktemp -d /tmp/o3k-p13-5e-real.XXXXXX); backend_port=19081; proxy_port=19082
password=${O3K_P13_PASSWORD:-p13-5e-provider-password}; project=eba29e2d-53de-461d-ae91-ede7402713cb
backend_pid=; proxy_pid=
proxy_evidence="$work/proxy-initial.json"
cleanup() { [[ -n "$proxy_pid" ]] && kill "$proxy_pid" 2>/dev/null || true; [[ -n "$backend_pid" ]] && kill "$backend_pid" 2>/dev/null || true; wait "$proxy_pid" 2>/dev/null || true; wait "$backend_pid" 2>/dev/null || true; }
trap cleanup EXIT
O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY=p13-5e-provider-token-signing-key-012345678901234567890123 \
  "$o3kd" --listen-addr "127.0.0.1:$backend_port" --data-dir "$work/data" >"$work/o3kd.log" 2>&1 & backend_pid=$!
for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$backend_port/readyz" >/dev/null 2>&1 && break; sleep .1; done
start_proxy() { python3 "$root_dir/scripts/p13_5e_fault_proxy.py" --serve-backend "http://127.0.0.1:$backend_port" --listen-port "$proxy_port" --evidence "$proxy_evidence" "$@" >"$work/proxy.address" 2>&1 & proxy_pid=$!; for _ in $(seq 1 50); do curl -fsS "http://127.0.0.1:$proxy_port/readyz" >/dev/null 2>&1 && return; sleep .1; done; }
stop_proxy() { kill "$proxy_pid" 2>/dev/null || true; wait "$proxy_pid" 2>/dev/null || true; proxy_pid=; }
mirror="$work/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"; mkdir -p "$mirror"; cp "$O3K_P13_PROVIDER_BINARY" "$mirror/terraform-provider-openstack_v3.4.0"; chmod 755 "$mirror/terraform-provider-openstack_v3.4.0"
cat >"$work/tofu.tfrc" <<EOF
provider_installation { filesystem_mirror { path = "$work/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] } direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] } }
EOF
cat >"$work/main.tf" <<EOF
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
  auth_url = "http://127.0.0.1:$proxy_port"
  user_name = "admin"
  password = "$password"
  tenant_id = "$project"
  max_retries = 0
}
resource "openstack_networking_network_v2" "network" { name = "p13-5e-network-a" }
EOF
export TF_CLI_CONFIG_FILE="$work/tofu.tfrc" TF_IN_AUTOMATION=1; cd "$work"
start_proxy; "$O3K_P13_TOFU" init -input=false -upgrade=false >/dev/null; "$O3K_P13_TOFU" apply -input=false -auto-approve >/dev/null; stop_proxy
proxy_evidence="$work/E1-read-response-loss.json"; start_proxy --rule 'GET /v2.0/networks* read_response_drop response_loss'; ! "$O3K_P13_TOFU" refresh >/dev/null 2>&1; stop_proxy; proxy_evidence="$work/E1-rerun.json"; start_proxy; "$O3K_P13_TOFU" refresh >/dev/null; stop_proxy
sed -i 's/p13-5e-network-a/p13-5e-network-b/' main.tf
proxy_evidence="$work/E2-pre-forward-update.json"; start_proxy --rule 'PUT /v2.0/networks* before_forward pre_forward_failure'; ! "$O3K_P13_TOFU" apply -input=false -auto-approve >/dev/null 2>&1; stop_proxy; proxy_evidence="$work/E2-rerun.json"; start_proxy; "$O3K_P13_TOFU" apply -input=false -auto-approve >/dev/null; stop_proxy
proxy_evidence="$work/E3-committed-update-response-loss.json"; start_proxy --rule 'PUT /v2.0/networks* after_commit_before_response response_loss'; ! "$O3K_P13_TOFU" apply -input=false -auto-approve >/dev/null 2>&1; stop_proxy; proxy_evidence="$work/E3-rerun.json"; start_proxy; "$O3K_P13_TOFU" refresh >/dev/null; stop_proxy
proxy_evidence="$work/E4-committed-delete-response-loss.json"; start_proxy --rule 'DELETE /v2.0/networks* after_commit_before_response response_loss'; ! "$O3K_P13_TOFU" destroy -input=false -auto-approve >/dev/null 2>&1; stop_proxy; proxy_evidence="$work/E4-rerun.json"; start_proxy; "$O3K_P13_TOFU" destroy -input=false -auto-approve >/dev/null; stop_proxy
cp "$work"/E*.json "${O3K_P13_EVIDENCE_DIR:-$work}/" 2>/dev/null || true
echo 'P13.5E real-provider fault-proxy lifecycle passed'
