#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="${O3K_REAL_HOST_ARTIFACT_DIR:-${ROOT_DIR}/target/real-host-workflow-artifacts}"
STATE_ROOT="${O3K_TESTLAB_STATE_ROOT:-}"
PROBE_FILE="${STATE_ROOT%/}/agent-inspect-probe.json"
RESULT_FILE="${ARTIFACT_DIR}/compute-agent-process-mtls-result.json"

mkdir -p -- "$ARTIFACT_DIR"
write_result() {
  local status="$1" reason="$2"
  python3 - "$RESULT_FILE" "$status" "$reason" <<'PY'
import json
import sys

path, status, reason = sys.argv[1:]
with open(path, "w", encoding="utf-8") as stream:
    json.dump({
        "artifact_type": "compute-agent-process-mtls",
        "redacted": True,
        "status": status,
        "reason": reason,
    }, stream, indent=2, sort_keys=True)
    stream.write("\n")
PY
}

if [[ -z "$STATE_ROOT" || "$STATE_ROOT" != /* || "$STATE_ROOT" == *..* ]]; then
  write_result failed "protected TestLab state root is unavailable"
  exit 1
fi
if ! command -v sudo >/dev/null 2>&1 || ! sudo -n true 2>/dev/null; then
  write_result failed "passwordless sudo is unavailable"
  exit 1
fi

for _ in $(seq 1 40); do
  sudo -n test -f "$PROBE_FILE" 2>/dev/null && break
  sleep 1
done
if ! sudo -n test -f "$PROBE_FILE" 2>/dev/null; then
  write_result failed "the real o3kd to o3k-compute inspect probe produced no evidence"
  exit 1
fi

temporary="$(mktemp "${RESULT_FILE}.tmp.XXXXXX")"
trap 'rm -f -- "$temporary"' EXIT
sudo -n cat "$PROBE_FILE" >"$temporary" \
  || { write_result failed "probe evidence could not be read"; exit 1; }
chmod 0600 "$temporary"
if python3 - "$temporary" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as stream:
    document = json.load(stream)
evidence = document.get("evidence", {})
if document.get("artifact_type") != "compute-agent-process-mtls":
    raise SystemExit("unexpected probe artifact type")
if document.get("status") != "passed" or document.get("redacted") is not True:
    raise SystemExit("probe did not pass in the real process boundary")
if document.get("scope") != "o3kd-to-o3k-compute-to-libvirt":
    raise SystemExit("probe scope is not the real process boundary")
if evidence.get("transport") != "mutual_tls":
    raise SystemExit("probe did not prove mutual TLS")
if evidence.get("command_state") != "accepted":
    raise SystemExit("probe command was not accepted")
if evidence.get("observation_state") != "failed_not_found":
    raise SystemExit("probe did not observe the expected absent-domain result")
PY
then
  mv -f -- "$temporary" "$RESULT_FILE"
  trap - EXIT
  echo "real compute-agent process-boundary mTLS inspect evidence passed"
  exit 0
fi

mv -f -- "$temporary" "$RESULT_FILE"
trap - EXIT
write_result failed "real process-boundary probe evidence did not satisfy the expected NotFound contract"
exit 1
