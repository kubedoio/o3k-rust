#!/usr/bin/env bash
set -Eeuo pipefail

# Regression test for the real-Cinder runner's machine-readable evidence
# emitter (Phase 13 artifacts). Extracts emit_evidence_artifacts from
# scripts/real-cinder-testbed-runner.sh and validates that every required
# artifact is written, valid JSON, redacted, and honest (no fabricated pass).

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-cinder-evidence.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

# Extract the emitter function body from the runner into a self-contained
# helper (the function's shell preamble plus the python heredoc).
awk '/^emit_evidence_artifacts\(\) {/,/^}$/' "${ROOT_DIR}/scripts/real-cinder-testbed-runner.sh" \
    > "${WORK_DIR}/emit.sh"

if [ ! -s "${WORK_DIR}/emit.sh" ]; then
    echo "emit_evidence_artifacts not found in runner" >&2
    exit 1
fi

export EVIDENCE_DIR="${WORK_DIR}/evidence"
export RUN_ID="evidence-test-1"
export GITHUB_SHA="0123456789abcdef0123456789abcdef01234567"
export VOLUME_ID="vol-test" SERVER_ID="srv-test"
export RELEASE_SERIES="2026.1" RELEASE_CODENAME="Gazpacho"
export CINDER_PYPI_PIN="28.0.0" CINDER_TEMPEST_PLUGIN_PIN="1.21.0"
mkdir -p "${EVIDENCE_DIR}"

# The extracted function calls emit_evidence_artifacts; source it and invoke.
bash -c 'source "$1"; emit_evidence_artifacts' _ "${WORK_DIR}/emit.sh"

REQUIRED=(
    real-cinder-environment.json
    keystone-hosted-service-result.json
    real-volume-lifecycle.json
    nova-cinder-attachment-result.json
    compute-block-device-result.json
    guest-device-observation.json
    attachment-restart-recovery.json
    real-cinder-cleanup-result.json
    foreign-state-result.json
    tempest-cinder-summary.json
    real-cinder-workflow-result.json
)

for artifact in "${REQUIRED[@]}"; do
    path="${EVIDENCE_DIR}/${artifact}"
    [ -f "${path}" ] || { echo "missing evidence artifact: ${artifact}" >&2; exit 1; }
    python3 - "${path}" <<'PY'
import json, sys
path = sys.argv[1]
with open(path, encoding="utf-8") as stream:
    doc = json.load(stream)
assert isinstance(doc, dict), f"{path} root is not an object"
assert doc.get("artifact_type") == path.rsplit("/", 1)[-1], f"{path} missing artifact_type"
assert doc.get("redacted") is True, f"{path} missing redaction marker"
assert doc.get("o3k_commit") == "0123456789abcdef0123456789abcdef01234567", f"{path} missing commit"
PY
done

# Honesty checks: Tempest summary must be not-executed (real Cinder not running
# here), foreign-state pending post-run guard, and no secret values present.
python3 - "${EVIDENCE_DIR}/tempest-cinder-summary.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "not-executed", doc["status"]
assert doc["evidence_tier"] == "tempest", doc["evidence_tier"]
PY
python3 - "${EVIDENCE_DIR}/foreign-state-result.json" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "pending-post-run-guard", doc["status"]
PY
if grep -rqE "cinder-service-password|cinder-db-password|O3K_PW=|DB_PW=|MQ_PW=" "${EVIDENCE_DIR}"; then
    echo "secret value leaked into evidence artifacts" >&2
    exit 1
fi

echo "real-cinder evidence artifact tests passed"
