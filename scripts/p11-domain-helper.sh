#!/bin/bash
# Tenant VM helper for the P11 real multi-host gate.
#
# Because o3k-compute attaches TAPs to its configured flat bridge (o3k-br0)
# rather than the P11 realm bridges (o3k-b-*), this helper creates the P11
# evidence VMs directly via virsh on each nested host.
#
# It reads the endpoint manifest produced by the Rust fabric driver and creates
# one CirrOS VM per endpoint, attached to the pre-realized realm TAP interface.

set -euo pipefail

LAB_ROOT="/var/lib/o3k-p11-lab"
FABRIC_STATE="${LAB_ROOT}/fabric-state"
MANIFEST="${FABRIC_STATE}/p11-endpoint-manifest.json"
IMAGES_DIR="${LAB_ROOT}/images"
SSH_KEY="/root/.ssh/id_ed25519"
SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"
VM_USER="cirros"

declare -A HOST_IP=(
  [p11h1]="10.77.0.11"
  [p11h2]="10.77.0.12"
  [p11h3]="10.77.0.13"
)

log() {
  echo "[$(date -Iseconds)] $*"
}

die() {
  echo "[ERROR] $*" >&2
  exit 1
}

require_manifest() {
  [[ -f "$MANIFEST" ]] || die "endpoint manifest not found: $MANIFEST"
}

remote() {
  local host="$1"
  shift
  ssh ${SSH_OPTS} -i "$SSH_KEY" "root@${HOST_IP[$host]}" "$@"
}

# Ensure the Debian cloud base image exists on the controller and on each host.
ensure_images() {
  local src="/var/lib/o3k-p11-lab/images/debian-12-generic-amd64.qcow2"
  [[ -f "$src" ]] || die "Debian cloud image not found: $src"
  mkdir -p "$IMAGES_DIR"
  cp -f "$src" "$IMAGES_DIR/base.qcow2"
  for host in p11h1 p11h2 p11h3; do
    remote "$host" "mkdir -p ${IMAGES_DIR}"
    rsync -e "ssh ${SSH_OPTS} -i ${SSH_KEY}" -az "$IMAGES_DIR/base.qcow2" "root@${HOST_IP[$host]}:${IMAGES_DIR}/base.qcow2" >/dev/null
  done
}

# Destroy and undefine a tenant VM on a host.
destroy_vm() {
  local host="$1" name="$2"
  remote "$host" "
    set -euo pipefail
    # Always attempt destroy/undefine; the conditional check sometimes misses
    # running domains with stale state, and a no-op undefine is harmless.
    virsh destroy '${name}' >/dev/null 2>&1 || true
    virsh undefine --remove-all-storage '${name}' >/dev/null 2>&1 || true
    rm -f '${LAB_ROOT}/vms/${name}.xml' '${LAB_ROOT}/vms/${name}-seed.iso' '${LAB_ROOT}/vms/${name}.qcow2'
  "
}

# Create a NoCloud config-drive ISO for a Debian cloud-init VM.
generate_seed_iso() {
  local host="$1" name="$2" ip="$3" gateway="$4" mtu="$5" ssh_key="$6" mac="$7"
  local dir="${LAB_ROOT}/vms/${host}-${name}"
  mkdir -p "$dir"

  cat > "$dir/meta-data" <<EOF
instance-id: ${name}-$(date +%s%N)
local-hostname: ${name}
EOF

  cat > "$dir/user-data" <<EOF
#cloud-config
hostname: ${name}
fqdn: ${name}.p11.local
users:
  - name: ${VM_USER}
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - ${ssh_key}
    lock_passwd: false
chpasswd:
  list: |
    ${VM_USER}:${name}-p11-gate
  expire: False
EOF

  cat > "$dir/network-config" <<EOF
version: 1
config:
  - type: physical
    name: eth0
    mac_address: '${mac}'
    subnets:
      - type: static
        address: ${ip}/24
        gateway: ${gateway}
        mtu: ${mtu}
EOF

  genisoimage -output "$dir/seed.iso" -volid cidata -joliet -rock \
    "$dir/meta-data" "$dir/user-data" "$dir/network-config" >/dev/null 2>&1
  echo "$dir/seed.iso"
}

# Create a small domain XML for a Debian cloud-init VM attached to a pre-created TAP.
generate_domain_xml() {
  local name="$1" tap="$2" mac="$3" disk="$4" seed="$5"
  cat <<EOF
<domain type='kvm'>
  <name>${name}</name>
  <memory unit='MiB'>256</memory>
  <vcpu>1</vcpu>
  <os>
    <type arch='x86_64' machine='pc'>hvm</type>
    <boot dev='hd'/>
  </os>
  <devices>
    <disk type='file' device='disk'>
      <driver name='qemu' type='qcow2'/>
      <source file='${disk}'/>
      <target dev='vda' bus='virtio'/>
    </disk>
    <disk type='file' device='cdrom'>
      <driver name='qemu' type='raw'/>
      <source file='${seed}'/>
      <target dev='hda' bus='ide'/>
      <readonly/>
    </disk>
    <interface type='ethernet'>
      <mac address='${mac}'/>
      <target dev='${tap}' managed='no'/>
      <model type='virtio'/>
    </interface>
    <serial type='pty'>
      <target port='0'/>
    </serial>
    <console type='pty'>
      <target type='serial' port='0'/>
    </console>
  </devices>
</domain>
EOF
}

# Start a tenant VM on the correct host.
start_vm() {
  local name="$1"
  require_manifest

  local entry
  entry=$(jq -e --arg n "$name" '.[] | select(.name == $n)' "$MANIFEST")
  [[ -n "$entry" ]] || die "endpoint $name not found in manifest"

  local host
  host=$(jq -r '.host' <<< "$entry")
  local tap
  tap=$(jq -r '.tap' <<< "$entry")
  local mac
  mac=$(jq -r '.mac' <<< "$entry")
  local fixed_ip
  fixed_ip=$(jq -r '.fixed_ip' <<< "$entry")
  local gateway
  gateway="${fixed_ip%.*}.1"

  destroy_vm "$host" "$name"

  log "Starting tenant VM $name on $host (tap=$tap, ip=$fixed_ip)"

  local pub_key
  pub_key=$(cat /root/.ssh/id_ed25519.pub)
  local seed_iso
  seed_iso=$(generate_seed_iso "$host" "$name" "$fixed_ip" "$gateway" "1400" "$pub_key" "$mac")

  local disk="${LAB_ROOT}/vms/${host}-${name}.qcow2"
  cp "$IMAGES_DIR/base.qcow2" "$disk"

  local xml
  xml=$(generate_domain_xml "$name" "$tap" "$mac" "$disk" "$seed_iso")
  local xml_path="${LAB_ROOT}/vms/${host}-${name}.xml"
  echo "$xml" > "$xml_path"

  # Copy disk, seed ISO, and XML to the host, then define and start.
  remote "$host" "mkdir -p ${LAB_ROOT}/vms $(dirname ${seed_iso})"
  rsync -e "ssh ${SSH_OPTS} -i ${SSH_KEY}" -az "$disk" "root@${HOST_IP[$host]}:${disk}" >/dev/null
  rsync -e "ssh ${SSH_OPTS} -i ${SSH_KEY}" -az "$seed_iso" "root@${HOST_IP[$host]}:${seed_iso}" >/dev/null
  rsync -e "ssh ${SSH_OPTS} -i ${SSH_KEY}" -az "$xml_path" "root@${HOST_IP[$host]}:${xml_path}" >/dev/null

  remote "$host" "
    set -euo pipefail
    virsh define '${xml_path}'
    virsh start '${name}'
  "

  log "Tenant VM $name started on $host"
}

# Get the serial console log for a tenant VM.
vm_console_log() {
  local name="$1"
  require_manifest
  local host
  host=$(jq -r --arg n "$name" '.[] | select(.name == $n) | .host' "$MANIFEST")
  remote "$host" "virsh console '${name}' --force" 2>&1 | head -200 || true
}

# Compute the realm network namespace name from a realm UUID.
realm_namespace() {
  local realm_id="$1"
  python3 -c "print('o3k-r-' + '${realm_id}'.replace('-', '')[:8])"
}

# Run a command inside a tenant VM via SSH from the host that owns it.
# The SSH originates in the realm namespace because the host default namespace
# is not on the tenant L2 fabric; the realm namespace holds the gateway.
vm_exec() {
  local name="$1"
  shift
  require_manifest
  local entry
  entry=$(jq -e --arg n "$name" '.[] | select(.name == $n)' "$MANIFEST")
  local host
  host=$(jq -r '.host' <<< "$entry")
  local ip
  ip=$(jq -r '.fixed_ip' <<< "$entry")
  local realm_id
  realm_id=$(jq -r '.realm_id' <<< "$entry")
  local ns
  ns=$(realm_namespace "$realm_id")
  remote "$host" "ip netns exec ${ns} ssh ${SSH_OPTS} -i ${SSH_KEY} ${VM_USER}@${ip} $(printf '%q ' "$@")"
}

# Wait until the VM responds to SSH from its binding host (inside realm ns).
wait_vm_ssh() {
  local name="$1"
  local entry
  entry=$(jq -e --arg n "$name" '.[] | select(.name == $n)' "$MANIFEST")
  local host
  host=$(jq -r '.host' <<< "$entry")
  local ip
  ip=$(jq -r '.fixed_ip' <<< "$entry")
  local realm_id
  realm_id=$(jq -r '.realm_id' <<< "$entry")
  local ns
  ns=$(realm_namespace "$realm_id")
  local deadline=$((SECONDS + 180))
  while ((SECONDS < deadline)); do
    if remote "$host" "ip netns exec ${ns} ssh ${SSH_OPTS} -i ${SSH_KEY} ${VM_USER}@${ip} true" >/dev/null 2>&1; then
      log "VM $name is reachable via SSH"
      return 0
    fi
    sleep 2
  done
  die "VM $name did not become reachable within 180s"
}

# Start all four P11 evidence VMs.
start_all_vms() {
  ensure_images
  for name in A1 A2 B1 B2; do
    start_vm "$name"
  done
  for name in A1 A2 B1 B2; do
    wait_vm_ssh "$name"
  done
  log "All P11 tenant VMs are running and reachable"
}

# Destroy all four P11 evidence VMs.
destroy_all_vms() {
  require_manifest || return 0
  for name in A1 A2 B1 B2; do
    local entry
    entry=$(jq -e --arg n "$name" '.[] | select(.name == $n)' "$MANIFEST" 2>/dev/null) || continue
    local host
    host=$(jq -r '.host' <<< "$entry")
    destroy_vm "$host" "$name" || true
  done
}

case "${1:-}" in
  start)
    start_all_vms
    ;;
  start-vm)
    [[ -n "${2:-}" ]] || die "usage: $0 start-vm <A1|A2|B1|B2>"
    start_vm "$2"
    wait_vm_ssh "$2"
    ;;
  console)
    [[ -n "${2:-}" ]] || die "usage: $0 console <A1|A2|B1|B2>"
    vm_console_log "$2"
    ;;
  exec)
    [[ -n "${2:-}" ]] || die "usage: $0 exec <A1|A2|B1|B2> <command>"
    name="$2"
    shift 2
    vm_exec "$name" "$@"
    ;;
  destroy)
    destroy_all_vms
    ;;
  destroy-vm)
    [[ -n "${2:-}" ]] || die "usage: $0 destroy-vm <A1|A2|B1|B2>"
    require_manifest
    host=$(jq -r --arg n "$2" '.[] | select(.name == $n) | .host' "$MANIFEST")
    destroy_vm "$host" "$2"
    ;;
  *)
    cat <<USAGE
Usage: $0 {start|start-vm <name>|console <name>|exec <name> <cmd>|destroy|destroy-vm <name>}
USAGE
    exit 1
    ;;
esac
