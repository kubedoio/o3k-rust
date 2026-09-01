#!/usr/bin/env bash
set -euo pipefail

: "${O3K_P13_TOFU:?set O3K_P13_TOFU to OpenTofu 1.12.6}"
: "${O3K_P13_PROVIDER_BINARY:?set O3K_P13_PROVIDER_BINARY to the unmodified provider 3.4.0 binary}"
: "${O3K_LVM_VOLUME_GROUP:?set a disposable LVM volume group}"
: "${O3K_LVM_THIN_POOL:?set a disposable LVM thin pool}"
: "${O3K_LVM_PROVIDER_NAMESPACE:?set a disposable LVM provider namespace}"
root_dir=$(cd "$(dirname "$0")/.." && pwd)
o3kd=${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}
password=${O3K_P13_PASSWORD:-p13-4-provider-password}
project_id=eba29e2d-53de-461d-ae91-ede7402713cb
port=${O3K_P13_PORT:-18992}
work=$(mktemp -d /tmp/o3k-p13-4-provider.XXXXXX)
pid=
validate_lvm_scope() {
    local vg_tags pool_tags expected_hash
    expected_hash="$(printf '%s' "$O3K_LVM_PROVIDER_NAMESPACE" | sha256sum | awk '{print $1}')"
    vg_tags="$(vgs --noheadings --options vg_tags --separator '|' "$O3K_LVM_VOLUME_GROUP" 2>/dev/null | tr -d '[:space:]')"
    pool_tags="$(lvs --noheadings --options lv_tags --separator '|' "$O3K_LVM_VOLUME_GROUP/$O3K_LVM_THIN_POOL" 2>/dev/null | tr -d '[:space:]')"
    [[ "$vg_tags" == "o3k_storage_$expected_hash" && "$pool_tags" == "o3k_pool_$expected_hash" ]] || {
        echo "refusing non-disposable LVM scope" >&2
        return 2
    }
}
validate_lvm_scope
cleanup() {
    if [[ -f "$work/terraform.tfstate" && -n "${O3K_P13_TOFU:-}" ]]; then
        (cd "$work" && TF_CLI_CONFIG_FILE="$work/tofu.tfrc" "$O3K_P13_TOFU" destroy -input=false -auto-approve >/dev/null 2>&1) || true
    fi
    if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi
}
trap cleanup EXIT

O3K_BOOTSTRAP_PASSWORD="$password" \
O3K_TOKEN_SIGNING_KEY="p13-4-provider-token-signing-key-012345678901234567890123" \
O3K_CINDER_PASSWORD="$password" \
O3K_CINDER_ENDPOINT="http://127.0.0.1:$port" \
O3K_LVM_VOLUME_GROUP="$O3K_LVM_VOLUME_GROUP" \
O3K_LVM_THIN_POOL="$O3K_LVM_THIN_POOL" \
O3K_LVM_PROVIDER_NAMESPACE="$O3K_LVM_PROVIDER_NAMESPACE" \
O3K_COMPATIBILITY_TRACE_PATH="$work/trace.jsonl" \
  "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work/data" >"$work/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do
    curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break
    sleep 0.1
done

mkdir -p "$work/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
cp "$O3K_P13_PROVIDER_BINARY" "$work/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
cat >"$work/tofu.tfrc" <<EOF
provider_installation {
  filesystem_mirror {
    path = "$work/mirror"
    include = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
  direct {
    exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"]
  }
}
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
  auth_url = "http://127.0.0.1:$port"
  user_name = "admin"
  password = "$password"
  tenant_id = "$project_id"
  max_retries = 0
}
resource "openstack_blockstorage_volume_v3" "probe" {
  name = "p13-4-provider-volume"
  description = "bounded native volume"
  size = 1
}
EOF
export TF_CLI_CONFIG_FILE="$work/tofu.tfrc" TF_IN_AUTOMATION=1
(cd "$work" && "$O3K_P13_TOFU" init -input=false -upgrade=false >/dev/null)
(cd "$work" && "$O3K_P13_TOFU" apply -input=false -auto-approve >/dev/null)
(cd "$work" && "$O3K_P13_TOFU" plan -detailed-exitcode >/dev/null)
(cd "$work" && "$O3K_P13_TOFU" destroy -input=false -auto-approve >/dev/null)
if [[ -n "${O3K_P13_4_DISCOVERY_TRACE:-}" && -f "$work/trace.jsonl" ]]; then
    cp "$work/trace.jsonl" "$O3K_P13_4_DISCOVERY_TRACE"
fi
echo "P13.4 native volume provider lifecycle passed"
