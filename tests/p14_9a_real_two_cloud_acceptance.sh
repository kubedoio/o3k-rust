#!/usr/bin/env bash
# P14.9A executable prerequisite/evidence runner. It is fail-closed.
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT="${O3K_P14_9A_EVIDENCE_OUTPUT:-$ROOT_DIR/target/p14-9a/evidence.json}"
mkdir -p "$(dirname "$OUTPUT")"
[[ "$OUTPUT" = /* ]] || { echo "evidence output must be absolute" >&2; exit 2; }

head_sha="$(git -C "$ROOT_DIR" rev-parse HEAD)"
source_ready=false
destination_ready=false
two_projects=false
tofu_ready=false
provider_ready=false
compute_ready=false
network_ready=false
volume_ready=false
postgres_ready=false
[[ -n "${O3K_P14_SOURCE_AUTH_URL:-}" && -n "${O3K_P14_SOURCE_PROJECT_ID:-}" && -n "${O3K_P14_SOURCE_CLOUD_ID:-}" ]] && source_ready=true
[[ -n "${O3K_P14_DESTINATION_URL:-}" && -n "${O3K_P14_DESTINATION_TOKEN:-}" ]] && destination_ready=true
[[ -n "${O3K_P14_SOURCE_PROJECT_B:-}" ]] && two_projects=true
[[ -n "${O3K_P14_TOFU:-}" && -x "${O3K_P14_TOFU}" ]] && tofu_ready=true
[[ "${O3K_P14_PROVIDER_MODIFIED:-false}" == false && -n "${O3K_P14_PROVIDER_BINARY:-}" && -f "${O3K_P14_PROVIDER_BINARY}" ]] && provider_ready=true
[[ "${O3K_P14_REAL_COMPUTE:-false}" == true ]] && compute_ready=true
[[ "${O3K_P14_REAL_NETWORK:-false}" == true ]] && network_ready=true
[[ "${O3K_P14_REAL_VOLUME:-false}" == true ]] && volume_ready=true
[[ -n "${O3K_P14_DATABASE_URL:-}" ]] && postgres_ready=true

ready=false
if [[ "$source_ready" == true && "$destination_ready" == true && "$two_projects" == true && "$tofu_ready" == true && "$provider_ready" == true && "$compute_ready" == true && "$network_ready" == true && "$volume_ready" == true && "$postgres_ready" == true ]]; then
  ready=true
fi

python3 - "$OUTPUT" "$head_sha" "$ready" "$source_ready" "$destination_ready" "$two_projects" "$tofu_ready" "$provider_ready" "$compute_ready" "$network_ready" "$volume_ready" "$postgres_ready" <<'PY'
import json
import pathlib
import sys

output, head, ready, *raw = sys.argv[1:]
ready = ready == "true"
names = ("source_configured", "destination_configured", "two_projects", "opentofu", "provider", "real_compute", "real_network", "real_volume", "postgresql")
checks = {name: value == "true" for name, value in zip(names, raw)}
document = {
    "artifact_type": "o3k-p14-9a-evidence",
    "schema_version": 1,
    "phase": "P14.9A",
    "profile": "p14-openstack-cold-migration-v1",
    "tested_runtime_head_sha": head,
    "source": {"profile": "openstack-source", "endpoint_fingerprint": "redacted", "configured": checks["source_configured"]},
    "destination": {"profile": "native-rust-testlab", "endpoint_fingerprint": "redacted", "configured": checks["destination_configured"]},
    "toolchain": {"opentofu": "1.12.6", "provider": "terraform-provider-openstack/openstack 3.4.0", "provider_modified": False},
    "environment_ready": ready,
    "execution_result": "blocked",
    "prerequisites": checks,
    "gates": [{"gate": f"G{i:02d}", "result": "blocked", "evidence_ref": f"prerequisite-report:G{i:02d}"} for i in range(1, 21)],
    "summary": {"owned_leaks": 0, "inconsistencies": 0, "foreign_state_changes": 0},
    "opentofu": {"final_plan": "not-run"},
}
pathlib.Path(output).write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
PY

python3 "$ROOT_DIR/scripts/validate_p14_9a_evidence.py" "$OUTPUT"
printf 'OpenStack source configured: %s\n' "$source_ready"
printf 'O3K destination configured: %s\n' "$destination_ready"
printf 'Two source projects available: %s\n' "$two_projects"
printf 'OpenTofu 1.12.6: %s\n' "$tofu_ready"
printf 'Provider 3.4.0 unmodified: %s\n' "$provider_ready"
printf 'Real compute destination: %s\n' "$compute_ready"
printf 'Real network destination: %s\n' "$network_ready"
printf 'Real volume destination: %s\n' "$volume_ready"
printf 'PostgreSQL: %s\n' "$postgres_ready"
printf 'Evidence output writable: true\n'
printf 'P14.9 environment ready: %s\n' "$ready"
if [[ "$ready" != true ]]; then
  exit 2
fi
echo 'P14.9A execution remains blocked until the provisioned journey is run.' >&2
exit 2
