#!/usr/bin/env bash
set -Eeuo pipefail

PROFILE=fake
DATA_DIR=/var/lib/o3k
while (($#)); do
  case "$1" in
    --profile) PROFILE="${2:?missing profile}"; shift 2;;
    --data-dir) DATA_DIR="${2:?missing data directory}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }
[[ "$DATA_DIR" == /* && "$DATA_DIR" != / ]] || { echo "data directory must be an absolute non-root path" >&2; exit 2; }

missing=()
for command in install sha256sum; do command -v "$command" >/dev/null 2>&1 || missing+=("$command"); done
if [[ "$PROFILE" == libvirt ]]; then
  for command in virsh qemu-img ip; do command -v "$command" >/dev/null 2>&1 || missing+=("$command"); done
  [[ -e /dev/kvm ]] || missing+=("/dev/kvm")
  virsh -c qemu:///system uri >/dev/null 2>&1 || missing+=("qemu:///system")
fi
if ((${#missing[@]})); then
  printf 'preflight failed (%s): missing %s\n' "$PROFILE" "$(IFS=', '; echo "${missing[*]}")" >&2
  exit 1
fi
SPACE_PATH="$DATA_DIR"
while [[ ! -e "$SPACE_PATH" && "$SPACE_PATH" != / ]]; do
  SPACE_PATH="${SPACE_PATH%/*}"
  [[ -n "$SPACE_PATH" ]] || SPACE_PATH=/
done
df -Pk "$SPACE_PATH" 2>/dev/null | awk '
  NR == 2 {
    found = 1
    if ($4 !~ /^[0-9]+$/) invalid = 1
    else if ($4 < 1048576) low = 1
  }
  END {
    if (!found || invalid) exit 2
    if (low) exit 1
  }
' || { echo "preflight failed: unable to verify at least 1 GiB free on the data filesystem ($SPACE_PATH)" >&2; exit 1; }
printf 'preflight passed: profile=%s data-filesystem=%s\n' "$PROFILE" "$SPACE_PATH"
