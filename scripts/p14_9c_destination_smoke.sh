#!/usr/bin/env bash
set -Eeuo pipefail

# Run the existing real-host provider smoke journey and project only the
# observations required by P14 readiness.  This is deliberately a separate
# executable boundary: the coordinator never treats API liveness as guest,
# packet, or storage evidence.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
evidence="${O3K_P14_DESTINATION_SMOKE_EVIDENCE:-$ROOT_DIR/target/p14-9c/destination-smoke.json}"
mkdir -p "$(dirname "$evidence")"

O3K_P13_7_EVIDENCE_OUTPUT="$evidence" \
  "$ROOT_DIR/tests/p13_7_real_host_iac_acceptance.sh" >/dev/null

python3 - "$evidence" <<'PY'
import json, sys

with open(sys.argv[1], encoding="utf-8") as stream:
    document = json.load(stream)
gates = {row.get("gate"): row for row in document.get("gates", [])}
passed = lambda gate: gates.get(gate, {}).get("result") == "passed"
json.dump({
    "compute_guest": passed("R3"),
    "packet_path": passed("R4"),
    "volume_persistence": passed("R5"),
}, sys.stdout, sort_keys=True)
print()
if not all((passed("R3"), passed("R4"), passed("R5"))):
    raise SystemExit(1)
PY
