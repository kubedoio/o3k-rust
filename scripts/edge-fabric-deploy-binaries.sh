#!/usr/bin/env bash
# Deploy O3K P11 release binaries, mTLS PKI, and per-host environment files to
# the multi-host lab VMs.
#
# Usage (as root on the deployment host):
#   /root/o3k-p11-fip-next/scripts/edge-fabric-deploy-binaries.sh
#
# The script rsyncs from /var/lib/o3k-fabric-lab/ over SSH as root using
# /root/.ssh/id_ed25519 and installs binaries into /opt/o3k/bin/ on each target.

set -euo pipefail

LAB_ROOT="/var/lib/o3k-fabric-lab"
SSH_KEY="/root/.ssh/id_ed25519"
SSH_OPTS=(-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o BatchMode=yes)

CONTROLLER_IP="10.77.0.1"
HOSTS=(p11h1 p11h2 p11h3)
HOST_IPS=(10.77.0.11 10.77.0.12 10.77.0.13)

# Sanity checks
if [[ ! -d "$LAB_ROOT/dist" ]]; then
  echo "error: release bundle not found at $LAB_ROOT/dist" >&2
  exit 1
fi
if [[ ! -d "$LAB_ROOT/pki" ]]; then
  echo "error: PKI not found at $LAB_ROOT/pki" >&2
  exit 1
fi
if [[ ! -f "$SSH_KEY" ]]; then
  echo "error: SSH key not found at $SSH_KEY" >&2
  exit 1
fi

rsync_ssh() {
  rsync -e "ssh ${SSH_OPTS[*]} -i $SSH_KEY" "$@"
}

remote() {
  local host="$1"
  shift
  ssh "${SSH_OPTS[@]}" -i "$SSH_KEY" "root@${host}" "$@"
}

install_binaries() {
  local host="$1"
  echo "[$host] installing release binaries to /opt/o3k/bin/"
  remote "$host" "
    set -euo pipefail
    mkdir -p /opt/o3k/bin /opt/o3k/share
    install -m 755 -o root -g root /opt/o3k/dist/o3kd /opt/o3k/bin/o3kd
    install -m 755 -o root -g root /opt/o3k/dist/o3k /opt/o3k/bin/o3k
    install -m 755 -o root -g root /opt/o3k/dist/o3k-compute-bin /opt/o3k/bin/o3k-compute-bin
    install -m 755 -o root -g root /opt/o3k/dist/o3k-network-bin /opt/o3k/bin/o3k-network-bin
    ln -sf o3k-compute-bin /opt/o3k/bin/o3k-compute
    ln -sf o3k-network-bin /opt/o3k/bin/o3k-network
    if [[ -f /opt/o3k/dist/OVMF_CODE_4M.fd ]]; then
      install -m 644 -o root -g root /opt/o3k/dist/OVMF_CODE_4M.fd /opt/o3k/share/OVMF_CODE_4M.fd
    fi
  "
}

install_pki() {
  local host="$1"
  echo "[$host] installing PKI to /opt/o3k/pki/"
  remote "$host" "mkdir -p /opt/o3k/pki"
  rsync_ssh -avz --delete "$LAB_ROOT/pki/" "root@${host}:/opt/o3k/pki/"
}

install_host_env() {
  local host="$1"
  local ip="$2"
  echo "[$host] installing environment template to /opt/o3k/env/${host}.env"
  remote "$ip" "mkdir -p /opt/o3k/env"
  rsync_ssh -avz "$LAB_ROOT/env/${host}.env" "root@${ip}:/opt/o3k/env/${host}.env"
}

# The compute agent expects its mTLS files under O3K_COMPUTE_TLS_DIR as
# ca.pem, agent.pem, and agent-key.pem. Create a host-scoped subdirectory so
# each host gets its own client identity.
prepare_compute_tls() {
  local host="$1"
  local ip="$2"
  echo "[$host] staging compute-agent TLS files in /opt/o3k/pki/${host}/"
  remote "$ip" "
    set -euo pipefail
    mkdir -p /opt/o3k/pki/${host}
    install -m 644 -o root -g root /opt/o3k/pki/ca.crt /opt/o3k/pki/${host}/ca.pem
    install -m 644 -o root -g root /opt/o3k/pki/${host}-client.crt /opt/o3k/pki/${host}/agent.pem
    install -m 600 -o root -g root /opt/o3k/pki/${host}-client.key /opt/o3k/pki/${host}/agent-key.pem
  "
}

# Controller: runs on the local deployment host; copy directly instead of SSH.
echo "[controller] deploying locally to /opt/o3k/"
mkdir -p /opt/o3k/bin /opt/o3k/pki /opt/o3k/env /opt/o3k/share
rsync -avz --delete "$LAB_ROOT/dist/" /opt/o3k/dist/
rsync -avz --delete "$LAB_ROOT/pki/" /opt/o3k/pki/
install -m 644 -o root -g root "$LAB_ROOT/env/controller.env" /opt/o3k/env/controller.env
install -m 755 -o root -g root /opt/o3k/dist/o3kd /opt/o3k/bin/o3kd
install -m 755 -o root -g root /opt/o3k/dist/o3k /opt/o3k/bin/o3k
install -m 755 -o root -g root /opt/o3k/dist/o3k-compute-bin /opt/o3k/bin/o3k-compute-bin
install -m 755 -o root -g root /opt/o3k/dist/o3k-network-bin /opt/o3k/bin/o3k-network-bin
ln -sf o3k-compute-bin /opt/o3k/bin/o3k-compute
ln -sf o3k-network-bin /opt/o3k/bin/o3k-network
if [[ -f /opt/o3k/dist/OVMF_CODE_4M.fd ]]; then
  install -m 644 -o root -g root /opt/o3k/dist/OVMF_CODE_4M.fd /opt/o3k/share/OVMF_CODE_4M.fd
fi

# Compute/network hosts: full bundle + per-host PKI subset + per-host env.
for i in "${!HOSTS[@]}"; do
  host="${HOSTS[$i]}"
  ip="${HOST_IPS[$i]}"
  echo "[$host] deploying to ${ip}"
  remote "$ip" "mkdir -p /opt/o3k/bin /opt/o3k/pki /opt/o3k/env /opt/o3k/share"
  rsync_ssh -avz --delete "$LAB_ROOT/dist/" "root@${ip}:/opt/o3k/dist/"
  install_pki "$ip"
  install_host_env "$host" "$ip"
  install_binaries "$ip"
  prepare_compute_tls "$host" "$ip"
done

echo "P11 deployment complete."
echo ""
echo "Next steps:"
echo "  1. On each host, edit /opt/o3k/env/<host>.env and set O3K_COMPUTE_UPLINK"
echo "     and O3K_NETWORK_UPLINK to the physical interface name."
echo "  2. On the controller, edit /opt/o3k/env/controller.env if you want to"
echo "     enable the single outbound network-agent dispatcher."
echo "  3. Start o3kd on the controller and the agents on the hosts using the"
echo "     environment files above."
