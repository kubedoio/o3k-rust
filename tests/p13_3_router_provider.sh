#!/usr/bin/env bash
set -euo pipefail
root_dir=$(cd "$(dirname "$0")/.." && pwd)
tofu=$O3K_P13_TOFU
provider=$O3K_P13_PROVIDER_BINARY
work=$(mktemp -d /tmp/o3k-p13-router.XXXXXX)
port=$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1]);s.close()')
password=$O3K_P13_PASSWORD
pid=
trap 'if test -n "$pid"; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi' EXIT
mirror=$work/mirror/registry.terraform.io/terraform-provider-openstack/openstack/3.4.0/linux_amd64
project=$work/project
mkdir -p "$mirror" "$project"
cp "$provider" "$mirror/terraform-provider-openstack_v3.4.0"
chmod 755 "$mirror/terraform-provider-openstack_v3.4.0"
cat > "$work/tofu.tfrc" <<EOF
provider_installation {
 filesystem_mirror { path = "$work/mirror" include = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
 direct { exclude = ["registry.terraform.io/terraform-provider-openstack/openstack"] }
}
EOF
cat > "$project/main.tf" <<EOF
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
  tenant_id = "eba29e2d-53de-461d-ae91-ede7402713cb"
  max_retries = 0
}
resource "openstack_networking_network_v2" "network" {
  name = "p13-router-network"
}
resource "openstack_networking_subnet_v2" "subnet" {
  network_id = openstack_networking_network_v2.network.id
  cidr = "10.77.0.0/24"
  ip_version = 4
  enable_dhcp = false
}
resource "openstack_networking_router_v2" "router" {
  name = "p13-router"
  admin_state_up = true
  external_network_id = openstack_networking_network_v2.network.id
  enable_snat = false
  depends_on = [openstack_networking_subnet_v2.subnet]
}
resource "openstack_networking_router_interface_v2" "interface" {
  router_id = openstack_networking_router_v2.router.id
  subnet_id = openstack_networking_subnet_v2.subnet.id
}
EOF
O3K_BOOTSTRAP_PASSWORD=$password O3K_TOKEN_SIGNING_KEY=p13-router-token-signing-key-012345678901234567890123 "$root_dir/target/debug/o3kd" --listen-addr 127.0.0.1:$port --data-dir $work/data >$work/o3kd.log 2>&1 &
pid=$!
for i in $(seq 1 120); do curl -fsS http://127.0.0.1:$port/readyz >/dev/null 2>&1 && break; sleep .1; done
cd "$project"
export TF_CLI_CONFIG_FILE=$work/tofu.tfrc TF_IN_AUTOMATION=1
$tofu init -input=false -upgrade=false >/dev/null
$tofu apply -auto-approve
router_id=$($tofu show -json | python3 -c 'import json,sys; print(next(x["values"]["id"] for x in json.load(sys.stdin)["values"]["root_module"]["resources"] if x["address"]=="openstack_networking_router_v2.router"))')
$tofu refresh
kill "$pid"; wait "$pid" 2>/dev/null || true; pid=
O3K_BOOTSTRAP_PASSWORD=$password O3K_TOKEN_SIGNING_KEY=p13-router-token-signing-key-012345678901234567890123 "$root_dir/target/debug/o3kd" --listen-addr 127.0.0.1:$port --data-dir $work/data >$work/o3kd-restart.log 2>&1 &
pid=$!
for i in $(seq 1 120); do curl -fsS http://127.0.0.1:$port/readyz >/dev/null 2>&1 && break; sleep .1; done
$tofu refresh
sed -i 's/name = "p13-router"/name = "p13-router-updated"/' "$project/main.tf"
$tofu apply -auto-approve
$tofu refresh
$tofu state rm openstack_networking_router_v2.router >/dev/null
$tofu import openstack_networking_router_v2.router "$router_id" >/dev/null
plan_status=0
$tofu plan -detailed-exitcode >/dev/null || plan_status=$?
if [[ "$plan_status" -ne 0 ]]; then
  echo "post-import Router plan is not converged (exit $plan_status)" >&2
  exit "$plan_status"
fi
$tofu destroy -target=openstack_networking_router_interface_v2.interface -auto-approve
$tofu destroy -parallelism=1 -auto-approve
echo "P13.3 Router provider lifecycle passed"
