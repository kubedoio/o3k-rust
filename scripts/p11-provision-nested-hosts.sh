#!/bin/bash
set -euo pipefail

# p11-provision-nested-hosts.sh
# Create three nested KVM/libvirt compute hosts for O3K P11 real multi-host gate.
# Re-runnable: destroys and rebuilds the three VMs on each invocation.

BASE_DIR="/var/lib/o3k-p11-lab"
BACKING_SRC="/root/noble-server-cloudimg-amd64.img"
BACKING="$BASE_DIR/noble-server-cloudimg-amd64.img"
SSH_KEY="/root/.ssh/id_ed25519"
REPORT="$BASE_DIR/provision-report.json"
BRIDGE="p11br0"
PUBKEY="ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIPUmR74sE06wZLnNkQ3umZ/kHVEUG4IQZPXcSTo7acXc root@nkudo-vm1"

declare -A VM_IP=(
  [p11h1]="10.77.0.11"
  [p11h2]="10.77.0.12"
  [p11h3]="10.77.0.13"
)

declare -A VM_MAC=(
  [p11h1]="52:54:00:77:00:11"
  [p11h2]="52:54:00:77:00:12"
  [p11h3]="52:54:00:77:00:13"
)

SSH_OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o ConnectTimeout=5 -o BatchMode=yes"

log() {
  echo "[$(date -Iseconds)] $*"
}

die() {
  echo "[ERROR] $*" >&2
  exit 1
}

cleanup_vm() {
  local name="$1"
  log "Cleaning up any existing VM '$name' ..."
  if virsh list --all --name | grep -qx "$name"; then
    virsh destroy "$name" >/dev/null 2>&1 || true
    virsh undefine "$name" >/dev/null 2>&1 || true
  fi
  rm -rf "$BASE_DIR/$name"
}

generate_cloudinit() {
  local name="$1" dir="$2" ip="$3" mac="${VM_MAC[$name]}"
  local inst_id="${name}-$(date +%s%N)"

  cat > "$dir/meta-data" <<EOF
instance-id: $inst_id
local-hostname: $name
EOF

  cat > "$dir/network-config" <<EOF
version: 2
ethernets:
  eth0:
    match:
      macaddress: "$mac"
    set-name: eth0
    dhcp4: false
    addresses:
      - $ip/24
    routes:
      - to: default
        via: 10.77.0.1
    nameservers:
      addresses:
        - 1.1.1.1
        - 8.8.8.8
EOF

  cat > "$dir/user-data" <<EOF
#cloud-config
hostname: $name
fqdn: $name.p11.local
manage_etc_hosts: true

users:
  - name: root
    ssh_authorized_keys:
      - $PUBKEY

ssh_pwauth: false
chpasswd:
  expire: false

package_update: true
package_upgrade: false
packages:
  - libvirt-daemon-system
  - qemu-system-x86
  - qemu-utils
  - lvm2
  - thin-provisioning-tools
  - ceph-common
  - wireguard-tools
  - nftables
  - dnsmasq-base
  - iproute2
  - python3
  - curl
  - jq
  - socat
  - netcat-openbsd
  - sudo
  - rsync
  - iptables
  - net-tools
  - bridge-utils

bootcmd:
  - [sh, -c, 'modprobe kvm && { grep -q AuthenticAMD /proc/cpuinfo && modprobe kvm_amd || true; grep -q GenuineIntel /proc/cpuinfo && modprobe kvm_intel || true; }']

EOF

  genisoimage -output "$dir/seed.iso" -volid cidata -joliet -rock \
    "$dir/meta-data" "$dir/user-data" "$dir/network-config" >/dev/null 2>&1
}

create_vm() {
  local name="$1"
  local ip="${VM_IP[$name]}"
  local mac="${VM_MAC[$name]}"
  local dir="$BASE_DIR/$name"
  local disk="$dir/root.qcow2"
  local seed="$dir/seed.iso"

  cleanup_vm "$name"
  mkdir -p "$dir"

  log "Creating overlay disk for '$name' ..."
  qemu-img create -f qcow2 -F qcow2 -b "$BACKING" "$disk" 12G >/dev/null

  log "Generating cloud-init seed ISO for '$name' ($ip) ..."
  generate_cloudinit "$name" "$dir" "$ip"

  log "Defining and starting VM '$name' ..."
  virt-install \
    --name "$name" \
    --memory 2560 \
    --vcpus 2 \
    --cpu host-passthrough \
    --import \
    --disk "path=$disk,format=qcow2,bus=virtio,cache=unsafe" \
    --disk "path=$seed,device=cdrom,bus=sata,readonly=on" \
    --network "bridge=$BRIDGE,model=virtio,mac=$mac" \
    --serial pty \
    --console pty \
    --graphics none \
    --noautoconsole \
    --os-variant ubuntu24.04 \
    --autostart \
    >/dev/null 2>&1

  # Ensure libvirt-qemu can access the overlay and seed ISO.
  chown libvirt-qemu:libvirt "$disk" "$seed" 2>/dev/null || true

  log "VM '$name' started."
}

wait_ssh() {
  local name="$1" ip="$2"
  log "Waiting for SSH on $name ($ip) ..."
  local deadline=$((SECONDS + 600))
  while ((SECONDS < deadline)); do
    if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" true >/dev/null 2>&1; then
      log "SSH on $name is up."
      return 0
    fi
    sleep 3
  done
  die "SSH on $name ($ip) did not become available within 600s."
}

wait_cloudinit() {
  local name="$1" ip="$2"
  log "Waiting for cloud-init to finish on $name ..."
  # cloud-init may exit non-zero on recoverable errors (e.g. bootcmd), so ignore status.
  ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "cloud-init status --wait" >/dev/null 2>&1 || true
  log "cloud-init finished on $name."
}

finalize_vm() {
  local name="$1" ip="$2"
  log "Finalizing users/polkit on $name ..."
  ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" bash -s <<'REMOTE'
set -euo pipefail
# Ensure KVM device is usable by the kvm group
chmod 0660 /dev/kvm
chown root:kvm /dev/kvm

# Create o3k service user (no shell) and o3k-compute runtime user.
# libvirt/qemu packages create libvirt and kvm groups; ensure qemu exists too.
getent group o3k >/dev/null || groupadd -g 10000 o3k
getent group o3k-compute >/dev/null || groupadd -g 10001 o3k-compute
getent group qemu >/dev/null || groupadd -r qemu
getent passwd o3k >/dev/null || useradd -u 10000 -g 10000 -d /var/lib/o3k -s /usr/sbin/nologin -m o3k
getent passwd o3k-compute >/dev/null || useradd -u 10001 -g 10001 -d /var/lib/o3k/compute -s /bin/bash -G libvirt,kvm,qemu -m o3k-compute

# Allow any member of the libvirt group to manage qemu:///system
cat > /etc/polkit-1/rules.d/50-o3k-compute-libvirt.rules <<'POLKIT'
polkit.addRule(function(action, subject) {
    if (action.id == "org.libvirt.unix.manage" && subject.isInGroup("libvirt")) {
        return polkit.Result.YES;
    }
});
POLKIT
chmod 644 /etc/polkit-1/rules.d/50-o3k-compute-libvirt.rules

# Ensure libvirtd is enabled and running
systemctl enable --now libvirtd || true
systemctl restart polkit || true
systemctl restart libvirtd || true
REMOTE
  log "$name finalized."
}

verify_vm() {
  local name="$1" ip="$2"
  log "Running verification checklist for $name ..."

  local hostname_ok="false"
  local nested_kvm_ok="false"
  local libvirtd_ok="false"
  local virsh_ok="false"
  local ip_ok="false"
  local internet_ok="false"
  local lvm_ok="false"

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "hostname" | grep -qx "$name"; then
    hostname_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "test -c /dev/kvm"; then
    nested_kvm_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "systemctl is-active libvirtd" >/dev/null 2>&1; then
    libvirtd_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "virsh list --all" >/dev/null 2>&1; then
    virsh_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "ip addr show" | grep -q "inet $ip/24"; then
    ip_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "ping -c1 -W5 1.1.1.1" >/dev/null 2>&1; then
    internet_ok="true"
  fi

  if ssh $SSH_OPTS -i "$SSH_KEY" "root@$ip" "vgs --noheadings" | wc -l | grep -qx "0"; then
    lvm_ok="true"
  fi

  log "$name: hostname=$hostname_ok nested_kvm=$nested_kvm_ok libvirtd=$libvirtd_ok virsh=$virsh_ok ip=$ip_ok internet=$internet_ok lvm_empty=$lvm_ok"

  printf '%s\n' "$hostname_ok $nested_kvm_ok $libvirtd_ok $virsh_ok $ip_ok $internet_ok $lvm_ok"
}

generate_report() {
  local -n h_ok=$1 n_ok=$2 l_ok=$3 i_ok=$4
  local names="p11h1 p11h2 p11h3"

  python3 - "$BASE_DIR" "$names" \
    "${VM_IP[p11h1]}" "${VM_IP[p11h2]}" "${VM_IP[p11h3]}" \
    "${VM_MAC[p11h1]}" "${VM_MAC[p11h2]}" "${VM_MAC[p11h3]}" \
    "${h_ok[p11h1]}" "${h_ok[p11h2]}" "${h_ok[p11h3]}" \
    "${n_ok[p11h1]}" "${n_ok[p11h2]}" "${n_ok[p11h3]}" \
    "${l_ok[p11h1]}" "${l_ok[p11h2]}" "${l_ok[p11h3]}" \
    "${i_ok[p11h1]}" "${i_ok[p11h2]}" "${i_ok[p11h3]}" <<'PY'
import sys, json, os, datetime
base_dir = sys.argv[1]
names = sys.argv[2].split()
ips = sys.argv[3:6]
macs = sys.argv[6:9]
ssh_ok = dict(zip(names, sys.argv[9:12]))
nested_ok = dict(zip(names, sys.argv[12:15]))
libvirtd_ok = dict(zip(names, sys.argv[15:18]))
internet_ok = dict(zip(names, sys.argv[18:21]))
report = {
    "vm_names": names,
    "ips": dict(zip(names, ips)),
    "macs": dict(zip(names, macs)),
    "root_disk_paths": {n: os.path.join(base_dir, n, "root.qcow2") for n in names},
    "ssh_check_ok": ssh_ok,
    "nested_kvm_ok": nested_ok,
    "libvirtd_ok": libvirtd_ok,
    "internet_ok": internet_ok,
    "timestamp": datetime.datetime.now(datetime.timezone.utc).isoformat().replace("+00:00", "Z"),
}
with open(os.path.join(base_dir, "provision-report.json"), "w") as f:
    json.dump(report, f, indent=2)
print(json.dumps(report, indent=2))
PY
}

main() {
  [[ -f "$BACKING_SRC" ]] || die "Backing image not found: $BACKING_SRC"
  [[ -f "$SSH_KEY" ]] || die "SSH private key not found: $SSH_KEY"
  command -v virt-install >/dev/null || die "virt-install not found"
  command -v virsh >/dev/null || die "virsh not found"
  command -v qemu-img >/dev/null || die "qemu-img not found"
  command -v genisoimage >/dev/null || die "genisoimage not found"

  mkdir -p "$BASE_DIR"
  chmod 755 "$BASE_DIR"

  # Hardlink the backing image into the lab directory so libvirt-qemu can read it
  # without exposing /root.  Same inode -> no extra disk space.
  if [[ ! -e "$BACKING" ]]; then
    ln "$BACKING_SRC" "$BACKING" || die "Failed to hardlink backing image"
  fi
  chmod 644 "$BACKING"

  log "Ensuring bridge $BRIDGE is up administratively ..."
  ip link set dev "$BRIDGE" up 2>/dev/null || true

  for name in p11h1 p11h2 p11h3; do
    create_vm "$name"
  done

  # Ensure bridge stays up after VM attachments.
  ip link set dev "$BRIDGE" up 2>/dev/null || true

  declare -A SSH_OK NESTED_OK LIBVIRTD_OK INTERNET_OK

  for name in p11h1 p11h2 p11h3; do
    wait_ssh "$name" "${VM_IP[$name]}"
  done

  for name in p11h1 p11h2 p11h3; do
    wait_cloudinit "$name" "${VM_IP[$name]}"
  done

  for name in p11h1 p11h2 p11h3; do
    finalize_vm "$name" "${VM_IP[$name]}"
  done

  for name in p11h1 p11h2 p11h3; do
    read h n l v i inter lvm < <(verify_vm "$name" "${VM_IP[$name]}" | tail -n1)
    # We only record the requested booleans; virsh and lvm are logged but not in report.
    SSH_OK[$name]="$h"
    NESTED_OK[$name]="$n"
    LIBVIRTD_OK[$name]="$l"
    INTERNET_OK[$name]="$i"
  done

  log "Generating report at $REPORT ..."
  generate_report SSH_OK NESTED_OK LIBVIRTD_OK INTERNET_OK

  log "Done."
}

main "$@"
