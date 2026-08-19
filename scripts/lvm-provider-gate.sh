#!/usr/bin/env bash
set -Eeuo pipefail

# Component-level LVM provider gate. It proves the provider object lifecycle
# and cleanup only; it is not the P10 real-guest acceptance workflow.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_LVM_ARTIFACT_DIR:-${ROOT_DIR}/target/lvm-provider-artifacts}"
mkdir -p -- "${ARTIFACT_DIR}"
OUTPUT="${ARTIFACT_DIR}/lvm-provider-smoke.json"
rm -f -- "${OUTPUT}"

: "${O3K_LVM_VOLUME_GROUP:?O3K_LVM_VOLUME_GROUP is required}"
: "${O3K_LVM_THIN_POOL:?O3K_LVM_THIN_POOL is required}"
: "${O3K_LVM_PROVIDER_NAMESPACE:?O3K_LVM_PROVIDER_NAMESPACE is required}"
: "${O3K_LVM_HOST_ID:?O3K_LVM_HOST_ID is required}"

cargo run --quiet --manifest-path "${ROOT_DIR}/Cargo.toml" \
    -p o3k-storage --example lvm-provider-smoke >"${OUTPUT}"

python3 - "${OUTPUT}" <<'PY'
import json, sys

value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["artifact_type"] == "lvm-provider-smoke", value
assert value["status"] == "passed", value
assert value["redacted"] is True, value
for key in ("owned_backend_leaks", "owned_attachment_leaks", "owned_inconsistencies", "foreign_mutations"):
    assert value[key] == 0, value
assert "/dev/" not in json.dumps(value), value
PY

echo "LVM provider component gate passed"
