#!/usr/bin/env bash
# ASR-022 one-line installer campaign — HOST runner (issue #613).
#
# Provisions a fresh nested-KVM VM with NO repo checkout and NO bundle copy,
# serves the local get.o3k.io endpoint shim (scripts/serve-installer-endpoint.py)
# and drives the two in-VM phases:
#   phase 1: canaries -> exact one-liner install -> assertions -> sudo reboot
#   phase 2: reboot recovery -> one-liner rerun idempotency -> teardown ->
#            uninstall -> one-liner reinstall -> lifecycle again -> purge ->
#            zero-residue + canaries -> evidence JSON
# The only things copied into the VM are in-vm-phase1.sh and in-vm-phase2.sh.
#
# Usage: bash host-run.sh <ubuntu|debian> <evidence-dir>
# Env overrides: O3K_CAMPAIGN_BUNDLE_DIST (default /tmp/campaign-tree/dist/...),
# O3K_CAMPAIGN_PORT (default 18000), O3K_CAMPAIGN_SOURCE_SHA (default HEAD).
set -Eeuo pipefail
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$SCRIPT_DIR/../.." && pwd)"
DISTRO="${1:-ubuntu}"
EVID="${2:-$REPO/target/real-host-workflow-artifacts/asr-022-$(git -C "$REPO" rev-parse --short HEAD)/one-line-$DISTRO}"
WORK="$REPO/target/asr-022-vms"
PORT="${O3K_CAMPAIGN_PORT:-18000}"
BUNDLE_DIST="${O3K_CAMPAIGN_BUNDLE_DIST:-/tmp/campaign-tree/dist/o3k-0.2.0-alpha.2}"
VERSION="${O3K_CAMPAIGN_VERSION:-v0.2.0-alpha.2}"
SOURCE_SHA="${O3K_CAMPAIGN_SOURCE_SHA:-$(git -C "$REPO" rev-parse HEAD)}"
SSH_KEY="$WORK/id_ed25519"
VM_NAME="asr022-${DISTRO}"
VM_EVID="/home/tester/o3k-campaign-evidence"
VM_SCRIPTS="/home/tester/o3k-campaign"
ENDPOINT_PID=""
mkdir -p "$WORK" "$EVID"
[ -f "$SSH_KEY" ] || ssh-keygen -t ed25519 -f "$SSH_KEY" -N '' -C "asr022" >/dev/null
SSH_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
  -o ConnectTimeout=5 -o ServerAliveInterval=15 -p 2322)
SCP_OPTS=(-i "$SSH_KEY" -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -P 2322)

log() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

cleanup() {
  log "cleanup"
  [ -n "$ENDPOINT_PID" ] && kill "$ENDPOINT_PID" 2>/dev/null || true
  if [ -f "$WORK/${VM_NAME}.pid" ]; then
    kill "$(cat "$WORK/${VM_NAME}.pid")" 2>/dev/null || true
  fi
  sleep 3
  rm -f "$WORK/${VM_NAME}.qcow2" "$WORK/${VM_NAME}-seed.iso" "$WORK/${VM_NAME}.pid"
  rm -rf "$WORK/seed-${VM_NAME}"
  log "cleanup done"
}
trap cleanup EXIT

case "$DISTRO" in
  ubuntu)
    BASE="/root/noble-server-cloudimg-amd64.img"
    if [ ! -f "$BASE" ]; then
      curl -L --retry 3 -o "$BASE" https://cloud-images.ubuntu.com/noble/current/noble-server-cloudimg-amd64.img
    fi
    ;;
  debian)
    BASE="$WORK/debian-12-genericcloud-amd64.qcow2"
    if [ ! -f "$BASE" ]; then
      curl -L --retry 3 -o "$BASE" https://cloud.debian.org/images/cloud/bookworm/latest/debian-12-genericcloud-amd64.qcow2
    fi
    ;;
  *) echo "unsupported distro: $DISTRO" >&2; exit 2;;
esac

# ---- provision the fresh VM ----------------------------------------------------
DISK="$WORK/${VM_NAME}.qcow2"
SEED_DIR="$WORK/seed-${VM_NAME}"
SEED_ISO="$WORK/${VM_NAME}-seed.iso"
rm -f "$DISK" "$SEED_ISO"
qemu-img create -f qcow2 -b "$BASE" -F qcow2 "$DISK" 20G
mkdir -p "$SEED_DIR"
# Minimal cloud-init set on purpose: curl + ca-certificates only. Every other
# dependency (qemu, libvirt, polkitd, openstack client, ...) must come from the
# one-liner itself — that is part of the acceptance.
cat > "$SEED_DIR/user-data" <<EOF
#cloud-config
hostname: ${VM_NAME}
users:
  - name: tester
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    ssh_authorized_keys:
      - $(cat "${SSH_KEY}.pub")
ssh_pwauth: false
package_update: true
package_upgrade: false
packages:
  - curl
  - ca-certificates
runcmd:
  - echo "tester ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/tester
  - chmod 0440 /etc/sudoers.d/tester
EOF
printf 'instance-id: %s-%s\nlocal-hostname: %s\n' "$VM_NAME" "$(date +%s)" "$VM_NAME" > "$SEED_DIR/meta-data"
genisoimage -output "$SEED_ISO" -volid cidata -joliet -rock \
  "$SEED_DIR/user-data" "$SEED_DIR/meta-data" >/dev/null

qemu-system-x86_64 \
  -name "$VM_NAME" \
  -machine type=q35,accel=kvm \
  -cpu host \
  -smp 2 \
  -m 3584 \
  -drive file="$DISK",if=virtio,format=qcow2 \
  -drive file="$SEED_ISO",if=virtio,media=cdrom \
  -netdev user,id=net0,hostfwd=tcp::2322-:22 \
  -device virtio-net-pci,netdev=net0 \
  -display none -daemonize -pidfile "$WORK/${VM_NAME}.pid"
log "VM ${VM_NAME} launched (slirp gateway 10.0.2.2 -> host)"

ssh_vm() { ssh "${SSH_OPTS[@]}" tester@localhost "$@"; }

for i in $(seq 1 60); do
  if ssh_vm true 2>/dev/null; then log "SSH up"; break; fi
  sleep 5
done
ssh_vm true || { echo "SSH never came up" >&2; exit 1; }
ssh_vm 'sudo timeout 600 cloud-init status --wait' 2>/dev/null || true
log "cloud-init finished"
log "nested KVM inside VM: $(ssh_vm 'ls /dev/kvm >/dev/null 2>&1 && echo present || echo absent')"
# Capture the boot identity BEFORE phase 1: the post-reboot gate below must
# prove a REAL reboot via a different /proc/sys/kernel/random/boot_id, not
# merely a short /proc/uptime (which a pre-reboot sshd could also satisfy).
BOOT_ID_BEFORE="$(ssh_vm 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null || true)"
[ -n "$BOOT_ID_BEFORE" ] || { echo "could not read pre-reboot boot_id" >&2; exit 1; }

# ---- start the local endpoint shim ---------------------------------------------
if ss -ltn 2>/dev/null | grep -q ":$PORT "; then
  echo "port $PORT is already in use" >&2; exit 1
fi
nohup python3 "$REPO/scripts/serve-installer-endpoint.py" \
  --port "$PORT" --bundle-dist "$BUNDLE_DIST" --version "$VERSION" \
  >"$EVID/endpoint.log" 2>&1 &
ENDPOINT_PID=$!
for i in $(seq 1 20); do
  curl -sf "http://127.0.0.1:$PORT/version" >/dev/null 2>&1 && break
  sleep 0.5
done
curl -sf "http://127.0.0.1:$PORT/version" >/dev/null \
  || { echo "endpoint shim did not come up" >&2; exit 1; }
log "endpoint shim serving on 0.0.0.0:$PORT (pid $ENDPOINT_PID)"

# ---- copy ONLY the two in-VM scripts (no repo, no bundle, no image) ------------
ssh_vm "mkdir -p $VM_SCRIPTS $VM_EVID"
scp "${SCP_OPTS[@]}" "$SCRIPT_DIR/in-vm-phase1.sh" "$SCRIPT_DIR/in-vm-phase2.sh" \
  tester@localhost:"$VM_SCRIPTS/"

# ---- phase 1: install through the one-liner, assert, reboot --------------------
log "phase 1: one-liner install"
set +e
ssh_vm "sudo bash $VM_SCRIPTS/in-vm-phase1.sh $DISTRO $VM_EVID $SOURCE_SHA" \
  | tee "$EVID/vm-${DISTRO}-phase1.log"
PHASE1_SSH=$?
set -e
grep -Fq 'PHASE1-COMPLETE' "$EVID/vm-${DISTRO}-phase1.log" || {
  echo "phase 1 did not complete (ssh exit $PHASE1_SSH); pulling diagnostics" >&2
  scp "${SCP_OPTS[@]}" -r tester@localhost:"$VM_EVID/." "$EVID/" 2>/dev/null || true
  ssh_vm 'sudo journalctl -u o3kd --no-pager -n 60; sudo journalctl -u o3k-compute --no-pager -n 60' \
    >"$EVID/vm-${DISTRO}-diagnostics.log" 2>&1 || true
  exit 1
}
log "phase 1 complete (VM is rebooting)"

# ---- wait for the VM to come back (host reboot recovery) -----------------------
# The VM keeps accepting SSH for tens of seconds after `sudo reboot` while
# systemd tears services down; a bare "SSH up" check therefore matches the
# PRE-reboot sshd and phase 2 gets killed when the reboot lands. Wait for SSH
# to actually go down first, then require a fresh boot (different boot_id
# captured below) before starting phase 2.
log "waiting for the phase-1 reboot to actually land (SSH must go down)"
WENT_DOWN=0
for i in $(seq 1 24); do
  if ! ssh_vm true 2>/dev/null && ! ssh_vm true 2>/dev/null; then WENT_DOWN=1; break; fi
  sleep 5
done
[ "$WENT_DOWN" = 1 ] || { echo "VM never went down after the phase-1 reboot" >&2; exit 1; }
log "VM is down; waiting for SSH + a NEW boot_id (up to 15 min)"
SSH_BACK=0
BOOT_ID_AFTER=""
BOOT_UPTIME=999999
for i in $(seq 1 180); do
  BOOT_ID_AFTER="$(ssh_vm 'cat /proc/sys/kernel/random/boot_id' 2>/dev/null || true)"
  BOOT_UPTIME="$(ssh_vm 'cut -d. -f1 /proc/uptime' 2>/dev/null || echo 999999)"
  if [ -n "$BOOT_ID_AFTER" ] && [ "$BOOT_ID_AFTER" != "$BOOT_ID_BEFORE" ]; then
    SSH_BACK=1; break
  fi
  sleep 5
done
[ "$SSH_BACK" = 1 ] || { echo "VM never came back with a new boot_id" >&2; exit 1; }
log "SSH up after reboot (boot_id changed, kernel uptime ${BOOT_UPTIME}s)"
# Ubuntu 24.04 mounts /tmp as tmpfs, so the campaign dirs live under the
# tester home (persistent across reboot, readable by the scp pull user).
ssh_vm "mkdir -p $VM_SCRIPTS $VM_EVID"

# ---- phase 2: recovery, idempotency, removal, purge, evidence ------------------
# Run detached inside the VM and poll a marker file over fresh SSH sessions:
# a dropped slirp session must not kill the evidence run.
DIAG_CMD='sudo free -m; sudo dmesg | tail -60; sudo journalctl -u o3kd --no-pager; sudo journalctl -u o3k-compute --no-pager; sudo journalctl -u ssh --no-pager -n 40; sudo ls -laR /var/lib/o3k 2>/dev/null | head -60; sudo ls -la /var/log/o3k 2>/dev/null; sudo tail -80 /var/log/o3k/*.log 2>/dev/null'
log "phase 2: recovery / rerun / removal / purge (detached + polled)"
ssh_vm "sudo rm -f $VM_EVID/phase2-done"
ssh_vm "sudo nohup bash $VM_SCRIPTS/in-vm-phase2.sh $DISTRO $VM_EVID $SOURCE_SHA \
  >$VM_EVID/phase2-console.log 2>&1 </dev/null &"
PHASE2_MARKER=""
for i in $(seq 1 150); do
  sleep 10
  PHASE2_MARKER="$(ssh_vm "sudo cat $VM_EVID/phase2-done 2>/dev/null" 2>/dev/null || true)"
  [ -n "$PHASE2_MARKER" ] && break
done
log "phase 2 poll result: ${PHASE2_MARKER:-<no marker yet>}"

# ---- pull evidence (always, success or failure) --------------------------------
scp "${SCP_OPTS[@]}" -r tester@localhost:"$VM_EVID/." "$EVID/" 2>/dev/null || true
[ -f "$EVID/phase2-console.log" ] && tail -40 "$EVID/phase2-console.log" \
  | tee -a "$EVID/vm-${DISTRO}-phase2.log"
if ! grep -Fq 'PHASE2-COMPLETE status=passed' <<<"$PHASE2_MARKER"; then
  echo "phase 2 failed or timed out: ${PHASE2_MARKER:-no marker}" >&2
  ssh_vm "$DIAG_CMD" >>"$EVID/vm-${DISTRO}-diagnostics.log" 2>&1 || true
  exit 1
fi
log "phase 2 complete"
cp "$EVID/one-line-${DISTRO}-install.json" "$EVID/../one-line-${DISTRO}-install.json" 2>/dev/null || true
log "evidence pulled to $EVID"
