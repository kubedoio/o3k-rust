#!/usr/bin/env bash
set -Eeuo pipefail

PROFILE=fake
DATA_DIR=/var/lib/o3k
CONFIG_DIR=/etc/o3k
while (($#)); do
  case "$1" in
    --profile) PROFILE="${2:?missing profile}"; shift 2;;
    --data-dir) DATA_DIR="${2:?missing data directory}"; shift 2;;
    --config-dir) CONFIG_DIR="${2:?missing config directory}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
"$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/preflight.sh" --profile "$PROFILE" || true
printf 'profile=%s\n' "$PROFILE"
printf 'data_dir=%s exists=%s\n' "$DATA_DIR" "$([[ -d "$DATA_DIR" ]] && echo yes || echo no)"
printf 'config_dir=%s exists=%s\n' "$CONFIG_DIR" "$([[ -d "$CONFIG_DIR" ]] && echo yes || echo no)"
if command -v systemctl >/dev/null 2>&1; then
  systemctl --no-pager --full status o3kd.service 2>&1 | sed -n '1,12p' || true
  [[ "$PROFILE" == libvirt ]] && systemctl --no-pager --full status o3k-compute.service 2>&1 | sed -n '1,12p' || true
fi
if [[ "$PROFILE" == libvirt ]] && command -v virsh >/dev/null 2>&1; then
  virsh -c qemu:///system list --all --name 2>&1 | sed '/^$/d' | sed -n '1,20p' || true
fi
