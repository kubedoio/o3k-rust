#!/usr/bin/env bash
set -Eeuo pipefail

# P14.9C disposable source cloud bootstrap.
#
# This creates only resources below the run-owned libvirt network/domain/state
# names.  It uses the upstream DevStack development deployment in an isolated
# Ubuntu cloud-image VM; it does not implement or emulate OpenStack services.
# Credentials are generated into the protected runtime state directory and are
# never written to the repository or printed.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
STATE_ROOT="${O3K_P14_9C_SOURCE_STATE:-/var/lib/o3k/p14-9c-source}"
NETWORK_NAME="p14-9c-source"
DOMAIN_NAME="p14-openstack-source"
NETWORK_XML="$STATE_ROOT/network.xml"
USER_DATA="$STATE_ROOT/user-data"
NETWORK_CONFIG="$STATE_ROOT/network-config"
SSH_KEY="$STATE_ROOT/ssh_ed25519"
ENV_FILE="$STATE_ROOT/source.env"
BASE_IMAGE="$STATE_ROOT/jammy-server-cloudimg-amd64.base.img"
IMAGE="$STATE_ROOT/jammy-server-cloudimg-amd64.img"
IMAGE_URL="https://cloud-images.ubuntu.com/jammy/current/jammy-server-cloudimg-amd64.img"
IMAGE_SHA_URL="https://cloud-images.ubuntu.com/jammy/current/SHA256SUMS"
SOURCE_IP="192.168.250.10"
GATEWAY_IP="192.168.250.1"

die() { echo "P14.9C source: $*" >&2; exit 2; }
need() { command -v "$1" >/dev/null 2>&1 || die "missing required tool: $1"; }

for tool in curl sha256sum qemu-img virsh virt-install ssh-keygen install openssl; do need "$tool"; done
[[ "$(id -u)" == 0 ]] || die "must run as root"
[[ -e /dev/kvm ]] || die "missing /dev/kvm"
virsh -c qemu:///system uri >/dev/null 2>&1 || die "qemu:///system unavailable"

make_state() {
    # libvirt-qemu must traverse the run directory, while credentials below it
    # remain private.  The state root contains no secrets itself.
    install -d -m 755 "$STATE_ROOT"
    if [[ ! -f "$SSH_KEY" ]]; then
        ssh-keygen -q -t ed25519 -N '' -f "$SSH_KEY" -C p14-9c-source
        chmod 600 "$SSH_KEY"
    fi
    if [[ ! -f "$BASE_IMAGE" ]]; then
        curl -fL --retry 3 -o "$BASE_IMAGE" "$IMAGE_URL"
        expected="$(curl -fsSL "$IMAGE_SHA_URL" | awk '/jammy-server-cloudimg-amd64.img$/ {print $1}')"
        [[ "$expected" =~ ^[0-9a-f]{64}$ ]] || die "Ubuntu image checksum was not published"
        actual="$(sha256sum "$BASE_IMAGE" | awk '{print $1}')"
        [[ "$actual" == "$expected" ]] || die "Ubuntu image checksum mismatch"
        chmod 0644 "$BASE_IMAGE"
    fi
    [[ -f "$IMAGE" ]] || cp --reflink=auto "$BASE_IMAGE" "$IMAGE"
    chmod 0644 "$IMAGE"
}

define_network() {
    if ! virsh -c qemu:///system net-info "$NETWORK_NAME" >/dev/null 2>&1; then
        cat >"$NETWORK_XML" <<EOF
<network>
  <name>$NETWORK_NAME</name>
  <forward mode='nat'/>
  <bridge name='virbr250' stp='on' delay='0'/>
  <ip address='$GATEWAY_IP' netmask='255.255.255.0'>
    <dhcp>
      <range start='192.168.250.100' end='192.168.250.200'/>
    </dhcp>
  </ip>
</network>
EOF
        virsh -c qemu:///system net-define "$NETWORK_XML"
        virsh -c qemu:///system net-start "$NETWORK_NAME"
        virsh -c qemu:///system net-autostart "$NETWORK_NAME"
    fi
}

write_cloud_init() {
    install -m 600 /dev/null "$USER_DATA"
    install -m 600 /dev/null "$NETWORK_CONFIG"
    cat >"$NETWORK_CONFIG" <<EOF
version: 2
ethernets:
  enp1s0:
    addresses: [$SOURCE_IP/24]
    routes:
      - to: default
        via: $GATEWAY_IP
    nameservers:
      addresses: [1.1.1.1, 8.8.8.8]
EOF
    cat >"$USER_DATA" <<EOF
#cloud-config
users:
  - default
  - name: stack
    gecos: P14 disposable DevStack user
    groups: [sudo]
    sudo: ["ALL=(ALL) NOPASSWD:ALL"]
    shell: /bin/bash
    ssh_authorized_keys:
      - $(<"$SSH_KEY.pub")
package_update: true
packages:
  - git
  - python3
  - python3-pip
  - python3-venv
  - bridge-utils
  - qemu-utils
write_files:
  - path: /opt/devstack-local.conf
    owner: root:root
    permissions: '0600'
    content: |
      [[local|localrc]]
      HOST_IP=$SOURCE_IP
      SERVICE_HOST=$SOURCE_IP
      ADMIN_PASSWORD=$ADMIN_PASSWORD
      DATABASE_PASSWORD=$DATABASE_PASSWORD
      RABBIT_PASSWORD=$RABBIT_PASSWORD
      SERVICE_PASSWORD=$SERVICE_PASSWORD
      disable_service horizon
      enable_service key
      enable_service n-api n-cpu n-cond n-sch n-novnc
      enable_service q-svc q-dhcp q-meta ovn-controller ovn-northd q-ovn-metadata-agent
      disable_service q-agt q-l3
      enable_service g-api g-reg
      enable_service c-api c-bak c-sch c-vol
      LIBVIRT_TYPE=qemu
      CINDER_CONFIGURE_LOVM=true
      ENABLE_VOLUME_MULTIATTACH=false
      LOGFILE=/opt/devstack/stack.log
runcmd:
  - [bash, -lc, "git clone --depth 1 --branch stable/2025.1 https://opendev.org/openstack/devstack /opt/devstack"]
  - [bash, -lc, "chown -R stack:stack /opt/devstack && install -o stack -g stack -m 0600 /opt/devstack-local.conf /opt/devstack/local.conf"]
  - [bash, -lc, "su - stack -c 'cd /opt/devstack && ./stack.sh' > /opt/devstack/cloud-init-stack.log 2>&1"]
EOF
}

create() {
    make_state
    define_network
    if virsh -c qemu:///system dominfo "$DOMAIN_NAME" >/dev/null 2>&1; then
        die "domain already exists; use status or cleanup"
    fi
    ADMIN_PASSWORD="$(openssl rand -hex 24)"
    DATABASE_PASSWORD="$(openssl rand -hex 24)"
    RABBIT_PASSWORD="$(openssl rand -hex 24)"
    SERVICE_PASSWORD="$(openssl rand -hex 24)"
    write_cloud_init
    qemu-img resize "$IMAGE" 130G >/dev/null
    umask 077
    {
        printf 'O3K_P14_SOURCE_AUTH_URL=http://%s/identity/v3\n' "$SOURCE_IP"
        printf 'O3K_P14_SOURCE_USERNAME=admin\nO3K_P14_SOURCE_USER_DOMAIN=Default\n'
        printf 'O3K_P14_SOURCE_PASSWORD=%s\n' "$ADMIN_PASSWORD"
        printf 'O3K_P14_SOURCE_ADMIN_PASSWORD=%s\n' "$ADMIN_PASSWORD"
        printf 'O3K_P14_SOURCE_REGION=RegionOne\nO3K_P14_SOURCE_CLOUD_ID=p14-openstack-source\n'
        printf 'O3K_P14_SOURCE_ALLOWED_HOSTS=%s\n' "$SOURCE_IP"
        printf 'O3K_P14_SOURCE_NETWORK_XML_SHA256=%s\n' "$(virsh -c qemu:///system net-dumpxml "$NETWORK_NAME" | sha256sum | awk '{print $1}')"
    } >"$ENV_FILE"
    chmod 600 "$ENV_FILE"
    virt-install --connect qemu:///system --name "$DOMAIN_NAME" --memory 20480 --vcpus 8 \
        --disk path="$IMAGE",format=qcow2,bus=virtio \
        --os-variant ubuntu22.04 --import --network network="$NETWORK_NAME",model=virtio \
        --graphics none --noautoconsole --cloud-init user-data="$USER_DATA",network-config="$NETWORK_CONFIG"
    printf 'O3K_P14_SOURCE_DOMAIN_UUID=%s\n' "$(virsh -c qemu:///system domuuid "$DOMAIN_NAME")" >>"$ENV_FILE"
    echo "P14.9C source VM created: $DOMAIN_NAME"
    echo "Protected runtime credentials: $ENV_FILE"
}

status() {
    virsh -c qemu:///system dominfo "$DOMAIN_NAME" 2>/dev/null || true
    virsh -c qemu:///system net-info "$NETWORK_NAME" 2>/dev/null || true
    [[ -f "$ENV_FILE" ]] && echo "protected credentials: $ENV_FILE"
}

cleanup() {
    [[ -f "$NETWORK_XML" ]] || die "refusing cleanup without run-owned network metadata"
    [[ -f "$ENV_FILE" ]] || die "refusing cleanup without run-owned environment metadata"
    expected_domain_uuid="$(awk -F= '$1 == "O3K_P14_SOURCE_DOMAIN_UUID" {print $2}' "$ENV_FILE")"
    expected_network_hash="$(awk -F= '$1 == "O3K_P14_SOURCE_NETWORK_XML_SHA256" {print $2}' "$ENV_FILE")"
    [[ "$expected_domain_uuid" =~ ^[0-9a-fA-F-]{36}$ ]] || die "invalid run-owned domain identity"
    [[ "$expected_network_hash" =~ ^[0-9a-f]{64}$ ]] || die "invalid run-owned network identity"
    if virsh -c qemu:///system dominfo "$DOMAIN_NAME" >/dev/null 2>&1; then
        [[ "$(virsh -c qemu:///system domuuid "$DOMAIN_NAME")" == "$expected_domain_uuid" ]] || die "domain identity mismatch"
        virsh -c qemu:///system destroy "$DOMAIN_NAME" 2>/dev/null || {
            virsh -c qemu:///system domstate "$DOMAIN_NAME" | grep -q '^shut off$' || die "could not stop run-owned VM"
        }
        virsh -c qemu:///system undefine "$DOMAIN_NAME" --remove-all-storage || die "could not remove run-owned VM"
    fi
    if virsh -c qemu:///system net-info "$NETWORK_NAME" >/dev/null 2>&1; then
        actual_network_hash="$(virsh -c qemu:///system net-dumpxml "$NETWORK_NAME" | sha256sum | awk '{print $1}')"
        [[ "$actual_network_hash" == "$expected_network_hash" ]] || die "network identity mismatch"
        virsh -c qemu:///system net-destroy "$NETWORK_NAME" || {
            virsh -c qemu:///system net-dumpxml "$NETWORK_NAME" >/dev/null || die "could not stop run-owned network"
        }
        virsh -c qemu:///system net-undefine "$NETWORK_NAME" || die "could not remove run-owned network"
    fi
    echo "P14.9C source owned VM/network removed; credentials and state retained at $STATE_ROOT"
}

case "${1:-status}" in
    create) create ;;
    status) status ;;
    cleanup) cleanup ;;
    *) die "usage: $0 {create|status|cleanup}" ;;
esac
