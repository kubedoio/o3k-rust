#!/usr/bin/env bash
set -euo pipefail
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tofu="${O3K_P13_TOFU:?O3K_P13_TOFU is required}"
o3kd="${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}"
password="${O3K_P13_PASSWORD:-p13-2d-disposable-password}"
project_id="eba29e2d-53de-461d-ae91-ede7402713cb"
port="${O3K_P13_PORT:-$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')}"
work_dir="$(mktemp -d /tmp/o3k-p13-2d.XXXXXX)"; project_dir="$work_dir/project"; o3kd_pid=""
mkdir -p "$project_dir"
cleanup() { [[ -z "$o3kd_pid" ]] || { kill "$o3kd_pid" 2>/dev/null || true; wait "$o3kd_pid" 2>/dev/null || true; }; rm -rf "$work_dir"; }
trap cleanup EXIT
start() {
  O3K_BOOTSTRAP_PASSWORD="$password" O3K_TOKEN_SIGNING_KEY="p13-2d-token-signing-key-012345678901234567890123" "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work_dir/data" >"$work_dir/o3kd.log" 2>&1 &
  o3kd_pid=$!; for _ in $(seq 1 120); do curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && return; sleep .1; done; cat "$work_dir/o3kd.log" >&2; exit 1
}
start
curl -fsS -D "$work_dir/auth.headers" -o /dev/null -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token="$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work_dir/auth.headers" | tr -d '\r')"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v2/images" --data '{"name":"p13-2d-image","visibility":"private","container_format":"bare","disk_format":"raw"}' >"$work_dir/image.json"
image_id="$(python3 -c 'import json,sys;print(json.load(open(sys.argv[1]))["id"])' "$work_dir/image.json")"
printf 'p13-2d-image-fixture\n' >"$work_dir/image-content"
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/octet-stream' --data-binary "@$work_dir/image-content" -X PUT "http://127.0.0.1:$port/v2/images/$image_id/file" >/dev/null
cat >"$project_dir/main.tf" <<EOF
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
data "openstack_images_image_v2" "image" { name = "p13-2d-image" }
data "openstack_compute_flavor_v2" "flavor" { name = "test.small" }
resource "openstack_networking_network_v2" "network" { name = "p13-2d-network" }
resource "openstack_networking_subnet_v2" "subnet" {
  network_id = openstack_networking_network_v2.network.id
  cidr = "198.51.102.0/24"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_compute_instance_v2" "server" {
  name = "p13-2d-server"
  image_id = data.openstack_images_image_v2.image.id
  flavor_id = data.openstack_compute_flavor_v2.flavor.id
  power_state = "active"
  force_delete = false
  stop_before_destroy = false
  tags = []
  network { uuid = openstack_networking_network_v2.network.id }
}
EOF
export TF_CLI_CONFIG_FILE="$work_dir/tofu.tfrc" TF_IN_AUTOMATION=1
mkdir -p "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64"
cp "${O3K_P13_PROVIDER_BINARY:?O3K_P13_PROVIDER_BINARY is required}" "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
chmod 0755 "$work_dir/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64/terraform-provider-openstack_v3.4.0"
cat >"$work_dir/tofu.tfrc" <<EOF
provider_installation { filesystem_mirror { path = "$work_dir/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] } direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] } }
EOF
cd "$project_dir"; run() { echo "== tofu $*"; "$tofu" "$@"; }
run init -input=false -upgrade=false; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/p13-2d-server/p13-2d-server-renamed/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
server_id="$($tofu show -json | python3 -c 'import json,sys; d=json.load(sys.stdin); print(next(r["values"]["id"] for r in d["values"]["root_module"]["resources"] if r["address"] == "openstack_compute_instance_v2.server"))')"
run state rm openstack_compute_instance_v2.server
run import openstack_compute_instance_v2.server "$server_id"
run apply -auto-approve
run plan -detailed-exitcode || [[ "$?" == 2 ]]
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' -X POST \
  "http://127.0.0.1:$port/v2.1/$project_id/servers/$server_id/action" \
  --data '{"reboot":{"type":"SOFT"}}' >/dev/null
run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/power_state = "active"/power_state = "shutoff"/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
sed -i 's/power_state = "shutoff"/power_state = "active"/' main.tf; run apply -auto-approve; run plan -detailed-exitcode || [[ "$?" == 2 ]]
kill "$o3kd_pid"; wait "$o3kd_pid" 2>/dev/null || true; o3kd_pid=""; start; run plan -detailed-exitcode || [[ "$?" == 2 ]]
run destroy -auto-approve
echo "P13.2D bounded server lifecycle passed"
