#!/usr/bin/env bash
set -euo pipefail

# PostgreSQL provider-level parity orchestrator.  The existing P13.5B journey
# is deliberately reused so the OpenTofu/provider boundary is identical to the
# portable evidence path.  This wrapper never promotes an unavailable or
# failed journey to PASS; callers receive a machine-readable blocked artifact.
root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
output="${O3K_P13_5F_POSTGRES_EVIDENCE_OUTPUT:-$root_dir/target/p13-5f/postgres-provider-parity.json}"
mkdir -p "$(dirname "$output")"

python3 - "$output" "$root_dir" <<'PY'
import json
import pathlib
import sys
from datetime import datetime, timezone

output = pathlib.Path(sys.argv[1])
root = pathlib.Path(sys.argv[2])
head = __import__("subprocess").check_output(
    ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
).strip()

scenarios = [
    "PG1-import-read-reconstruction",
    "PG2-mutable-drift-reconvergence",
    "PG3-remote-deletion-recreation",
    "PG4-independent-replacement",
    "PG5-router-interface-relationship",
    "PG6-volume-attachment-relationship",
    "PG7-operation-replay-unknown-outcome",
]
document = {
    "artifact_type": "o3k-p13-5f-postgres-provider-parity",
    "schema_version": 1,
    "phase": "P13.5F",
    "tested_o3k_head_sha": head,
    "generated_at": datetime.now(timezone.utc).isoformat(),
    "backend": "postgresql",
    "provider_modified": False,
    "execution": {
        "orchestrator": "tests/p13_5f_postgres_provider_parity.sh",
        "reused_journey": "tests/p13_5b_refresh_import.sh",
        "status": "not_run",
    },
    "scenarios": [
        {
            "scenario": name,
            "result": "not_run",
            "externally_equivalent": False,
            "reason": "provider-level journey has not executed",
        }
        for name in scenarios
    ],
    "final_verdict": "blocked",
}
output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n")
print(json.dumps({"output": str(output), "status": "blocked"}))
PY
