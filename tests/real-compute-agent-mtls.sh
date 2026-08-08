#!/usr/bin/env bash
set -Eeuo pipefail

ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-target/real-host-workflow-artifacts}"
mkdir -p "${ARTIFACT_DIR}"
OUTPUT_PATH="${ARTIFACT_DIR}/compute-agent-mtls-result.json"
LOG_PATH="${ARTIFACT_DIR}/.compute-agent-mtls.log"

write_result() {
    local status="$1" reason="$2" evidence_json="${3:-null}"
    python3 - "${OUTPUT_PATH}" "${status}" "${reason}" "${evidence_json}" <<'PY'
import json
import sys
import time

path, status, reason, evidence = sys.argv[1:]
result = {
    "artifact_type": "compute-agent-mtls",
    "status": status,
    "reason": reason,
    "redacted": True,
    "scope": "agent-provider-to-control-plane-to-agent-protocol",
    "finished_at": int(time.time()),
}
if evidence != "null":
    result["evidence"] = json.loads(evidence)
with open(path, "w", encoding="utf-8") as stream:
    json.dump(result, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY
}

if ! output=$(cargo test -p o3k-compute-agent --test agent_mtls --all-features -- --nocapture 2>"${LOG_PATH}"); then
    write_result failed "mTLS agent integration test failed"
    cat "${LOG_PATH}" >&2
    exit 1
fi

evidence_line="$(printf '%s\n' "${output}" | sed -n 's/^O3K_AGENT_MTLS_EVIDENCE=//p' | tail -n 1)"
if [[ -z "${evidence_line}" ]]; then
    write_result failed "mTLS test produced no machine-readable evidence"
    exit 1
fi
write_result passed "mTLS command acceptance and observation completed" "${evidence_line}"
