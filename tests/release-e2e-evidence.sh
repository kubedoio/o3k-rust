#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VALIDATOR="${ROOT_DIR}/scripts/validate-release-e2e-evidence.py"
SCHEMA="${ROOT_DIR}/contracts/release-e2e-evidence.schema.json"
HARNESS="${ROOT_DIR}/tests/openstack-cli-libvirt.sh"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-release-e2e-evidence.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

fail() {
    echo "release E2E evidence contract test failed: $1" >&2
    exit 1
}

# Positive: the schema-driven example artifact is accepted by the same
# validator the release gate and the producer harness consume.
python3 "${VALIDATOR}" --example >"${WORK_DIR}/example.json" \
    || fail "validator --example did not produce an artifact"
python3 "${VALIDATOR}" "${WORK_DIR}/example.json" \
    || fail "validator rejected the schema-driven example artifact"
echo "schema-driven example artifact passes the validator"

# Drift regression: the producer's RESOURCE_KEYS/CLEANUP_KEYS constants must
# equal the schema's required sets. Changing either side without the other
# fails here; no hand-written fixture participates in this assertion.
python3 - "${HARNESS}" "${SCHEMA}" <<'PY' || fail "producer constants drifted from the contract schema"
import ast
import json
import pathlib
import sys

harness_text = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
schema = json.loads(pathlib.Path(sys.argv[2]).read_text(encoding="utf-8"))
expected_resources = schema["properties"]["resources"]["required"]
expected_cleanup = (
    schema["properties"]["cleanup"]["properties"]["resources"]["required"]
)

def parse_constant(name):
    marker = f"{name} = "
    line = next(
        candidate for candidate in harness_text.splitlines()
        if candidate.startswith(marker)
    )
    return tuple(ast.literal_eval(line[len(marker):].strip()))

if sorted(parse_constant("RESOURCE_KEYS")) != sorted(expected_resources):
    raise SystemExit("RESOURCE_KEYS does not match the schema's resources required set")
if sorted(parse_constant("CLEANUP_KEYS")) != sorted(expected_cleanup):
    raise SystemExit("CLEANUP_KEYS does not match the schema's cleanup required set")
PY
echo "producer resource constants match the contract schema"

# Structural negatives: each mutated artifact must be rejected by the
# validator with a specific error.
expect_reject() {
    local name="$1" expected_error="$2"
    if python3 "${VALIDATOR}" "${WORK_DIR}/${name}.json" \
        >"${WORK_DIR}/${name}.out" 2>&1; then
        fail "validator accepted ${name}"
    fi
    grep -q "${expected_error}" "${WORK_DIR}/${name}.out" \
        || fail "validator rejection of ${name} lacks: ${expected_error}"
}

python3 - "${WORK_DIR}/missing-port-id.json" "${WORK_DIR}/example.json" <<'PY'
import json, pathlib, sys
path, source = map(pathlib.Path, sys.argv[1:])
value = json.loads(source.read_text(encoding="utf-8"))
del value["resources"]["port_id"]
path.write_text(json.dumps(value), encoding="utf-8")
PY
expect_reject missing-port-id "resources: missing required keys: port_id"

python3 - "${WORK_DIR}/missing-port-cleanup.json" "${WORK_DIR}/example.json" <<'PY'
import json, pathlib, sys
path, source = map(pathlib.Path, sys.argv[1:])
value = json.loads(source.read_text(encoding="utf-8"))
del value["cleanup"]["resources"]["port"]
path.write_text(json.dumps(value), encoding="utf-8")
PY
expect_reject missing-port-cleanup "cleanup.resources: missing required keys: port"

python3 - "${WORK_DIR}/bad-vocabulary.json" "${WORK_DIR}/example.json" <<'PY'
import json, pathlib, sys
path, source = map(pathlib.Path, sys.argv[1:])
value = json.loads(source.read_text(encoding="utf-8"))
value["cleanup"]["resources"]["image"] = "destroyed"
path.write_text(json.dumps(value), encoding="utf-8")
PY
expect_reject bad-vocabulary "cleanup.resources.image: must be one of: verified_absent, not_verified, pending"

python3 - "${WORK_DIR}/extra-resource-key.json" "${WORK_DIR}/example.json" <<'PY'
import json, pathlib, sys
path, source = map(pathlib.Path, sys.argv[1:])
value = json.loads(source.read_text(encoding="utf-8"))
value["resources"]["volume_id"] = "00000000-0000-0000-0000-0000000000ff"
path.write_text(json.dumps(value), encoding="utf-8")
PY
expect_reject extra-resource-key "resources: unexpected keys: volume_id"

python3 - "${WORK_DIR}/extra-cleanup-key.json" "${WORK_DIR}/example.json" <<'PY'
import json, pathlib, sys
path, source = map(pathlib.Path, sys.argv[1:])
value = json.loads(source.read_text(encoding="utf-8"))
value["cleanup"]["resources"]["volume"] = "verified_absent"
path.write_text(json.dumps(value), encoding="utf-8")
PY
expect_reject extra-cleanup-key "cleanup.resources: unexpected keys: volume"

echo "release E2E evidence contract tests passed"
