#!/usr/bin/env bash
set -Eeuo pipefail

PROFILE=fake
while (($#)); do
  case "$1" in
    --profile) PROFILE="${2:?missing profile}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }

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
df -Pk /var/lib 2>/dev/null | awk 'NR==2 { if ($4 < 1048576) exit 1 }' || { echo "preflight failed: less than 1 GiB free on /var/lib" >&2; exit 1; }
printf 'preflight passed: profile=%s\n' "$PROFILE"
