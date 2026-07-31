#!/usr/bin/env bash
set -Eeuo pipefail

if (($# != 1)); then
    echo "usage: $0 OUTPUT_JSON" >&2
    exit 2
fi

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 "${ROOT_DIR}/scripts/real-host-owned-inventory.py" "$1"
