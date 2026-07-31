#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ARTIFACT_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-release-gate.XXXXXX")"
trap 'rm -rf -- "${ARTIFACT_DIR}"' EXIT

python3 - "${ARTIFACT_DIR}" <<'PY'
import hashlib, json, pathlib, sys, time

root = pathlib.Path(sys.argv[1])
finished_at = int(time.time())
common = {
    "profile": "libvirt",
    "redacted": True,
    "cleanup": {"status": "passed"},
    "finished_at": finished_at,
}
artifacts = {
    "e2e.json": {
        **common,
        "artifact_type": "openstack-cli-e2e",
        "status": "passed",
        "public_api_only": True,
        "lifecycle": {name: True for name in ("create", "show", "list", "stop", "start", "reboot", "console", "delete")},
    },
    "ubuntu.json": {**common, "artifact_type": "clean-install", "status": "passed", "distro": "ubuntu", "install": {"status": "passed"}},
    "debian.json": {**common, "artifact_type": "clean-install", "status": "passed", "distro": "debian", "install": {"status": "passed"}},
    "recovery.json": {**common, "artifact_type": "failure-recovery", "status": "passed", "failures": ["agent-restart"]},
    "benchmark-raw.json": {**common, "artifact_type": "benchmark", "status": "measured", "environment": {"uname": "Linux test-host 6.1", "rustc": "rustc 1.85.0"}, "samples": 5, "control_plane": {"startup_readiness_ms": 100, "token_p95_seconds": 0.01, "idle_rss_kib": 1024}, "guest_and_libvirt": {"status": "measured"}, "targets": {"startup_readiness_ms": 2000, "idle_rss_mib": 150, "token_p95_ms": 100}},
}
raw = artifacts["benchmark-raw.json"]
(root / "benchmark-raw.json").write_text(json.dumps(raw), encoding="utf-8")
raw_sha256 = hashlib.sha256(json.dumps(raw, sort_keys=True, separators=(",", ":"), ensure_ascii=True).encode()).hexdigest()
artifacts["benchmark.json"] = {**common, "artifact_type": "benchmark", "status": "measured", "samples": raw["samples"], "control_plane": raw["control_plane"], "guest_and_libvirt": raw["guest_and_libvirt"], "targets_evaluated": {"startup": True, "rss": True, "token_p95": True}, "raw_sha256": raw_sha256}
for name, value in artifacts.items():
    (root / name).write_text(json.dumps(value), encoding="utf-8")
PY

OUTPUT="${ARTIFACT_DIR}/valid-release.json"
bash "${ROOT_DIR}/packaging/release-gate.sh" \
    --e2e "${ARTIFACT_DIR}/e2e.json" \
    --install-ubuntu "${ARTIFACT_DIR}/ubuntu.json" \
    --install-debian "${ARTIFACT_DIR}/debian.json" \
    --recovery "${ARTIFACT_DIR}/recovery.json" \
    --benchmark "${ARTIFACT_DIR}/benchmark.json" \
    --benchmark-raw "${ARTIFACT_DIR}/benchmark-raw.json" \
    --output "${OUTPUT}"
grep -q '"status": "ready"' "${OUTPUT}"

if bash "${ROOT_DIR}/packaging/release-gate.sh" \
    --e2e "${ARTIFACT_DIR}/e2e.json" \
    --install-ubuntu "${ARTIFACT_DIR}/ubuntu.json" \
    --install-debian "${ARTIFACT_DIR}/debian.json" \
    --recovery "${ARTIFACT_DIR}/recovery.json" \
    --benchmark-raw "${ARTIFACT_DIR}/benchmark-raw.json" \
    --output "${ARTIFACT_DIR}/missing-benchmark.json"; then
    echo "release gate accepted missing benchmark evidence" >&2
    exit 1
fi
grep -q 'benchmark: artifact path was not supplied' "${ARTIFACT_DIR}/missing-benchmark.json"

GATE_ARGS=(
    --e2e "${ARTIFACT_DIR}/e2e.json"
    --install-ubuntu "${ARTIFACT_DIR}/ubuntu.json"
    --install-debian "${ARTIFACT_DIR}/debian.json"
    --recovery "${ARTIFACT_DIR}/recovery.json"
    --benchmark "${ARTIFACT_DIR}/benchmark.json"
    --benchmark-raw "${ARTIFACT_DIR}/benchmark-raw.json"
)

set_e2e_finished_at() {
    python3 - "${ARTIFACT_DIR}/e2e.json" "$1" <<'PY'
import json, pathlib, sys

path = pathlib.Path(sys.argv[1])
value = json.loads(path.read_text(encoding="utf-8"))
value["finished_at"] = int(sys.argv[2])
path.write_text(json.dumps(value), encoding="utf-8")
PY
}

set_e2e_finished_at "$(python3 - <<'PY'
import time
print(int(time.time()) - 3_601)
PY
)"
if O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS=3600 bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/stale-release.json"; then
    echo "release gate accepted stale evidence" >&2
    exit 1
fi
grep -q 'older than the configured maximum age' "${ARTIFACT_DIR}/stale-release.json"

set_e2e_finished_at -1
if O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS=3600 bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/negative-release.json"; then
    echo "release gate accepted a negative evidence timestamp" >&2
    exit 1
fi
grep -q 'finished_at must be positive' "${ARTIFACT_DIR}/negative-release.json"

set_e2e_finished_at 0
if O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS=3600 bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/zero-release.json"; then
    echo "release gate accepted a zero evidence timestamp" >&2
    exit 1
fi
grep -q 'finished_at must be positive' "${ARTIFACT_DIR}/zero-release.json"

set_e2e_finished_at "$(python3 - <<'PY'
import time
# Keep the future timestamp comfortably beyond the gate's validation boundary.
print(int(time.time()) + 3600)
PY
)"
if O3K_RELEASE_EVIDENCE_MAX_AGE_SECONDS=3600 bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/future-release.json"; then
    echo "release gate accepted future-dated evidence" >&2
    exit 1
fi
grep -q 'finished_at cannot be in the future' "${ARTIFACT_DIR}/future-release.json"

python3 - "${ARTIFACT_DIR}/e2e.json" <<'PY'
import json, sys
with open(sys.argv[1], "w", encoding="utf-8") as output:
    json.dump({"status": "passed"}, output)
PY
if bash "${ROOT_DIR}/packaging/release-gate.sh" \
    --e2e "${ARTIFACT_DIR}/e2e.json" \
    --install-ubuntu "${ARTIFACT_DIR}/ubuntu.json" \
    --install-debian "${ARTIFACT_DIR}/debian.json" \
    --recovery "${ARTIFACT_DIR}/recovery.json" \
    --benchmark "${ARTIFACT_DIR}/benchmark.json" \
    --benchmark-raw "${ARTIFACT_DIR}/benchmark-raw.json" \
    --output "${ARTIFACT_DIR}/invalid-release.json"; then
    echo "release gate accepted an underspecified artifact" >&2
    exit 1
fi

python3 - "${ARTIFACT_DIR}/benchmark-raw.json" <<'PY'
import json, sys
path = sys.argv[1]
value = json.loads(open(path, encoding="utf-8").read())
del value["environment"]["rustc"]
open(path, "w", encoding="utf-8").write(json.dumps(value))
PY
if bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/missing-environment.json"; then
    echo "release gate accepted raw benchmark without required environment metadata" >&2
    exit 1
fi
grep -q 'benchmark_raw: environment.rustc must be a non-empty string' "${ARTIFACT_DIR}/missing-environment.json"

python3 - "${ARTIFACT_DIR}/benchmark-raw.json" <<'PY'
import json, sys
path = sys.argv[1]
value = json.loads(open(path, encoding="utf-8").read())
value["environment"]["rustc"] = "rustc 1.85.0"
value["samples"] = 6
open(path, "w", encoding="utf-8").write(json.dumps(value))
PY
if bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/inconsistent-release.json"; then
    echo "release gate accepted inconsistent benchmark summary/raw" >&2
    exit 1
fi
grep -q 'benchmark: samples must match benchmark_raw.samples' "${ARTIFACT_DIR}/inconsistent-release.json"

python3 - "${ARTIFACT_DIR}/benchmark-raw.json" <<'PY'
import json, sys
path = sys.argv[1]
value = json.loads(open(path, encoding="utf-8").read())
value["samples"] = 5
value["control_plane"]["startup_readiness_ms"] = 101
open(path, "w", encoding="utf-8").write(json.dumps(value))
PY
if bash "${ROOT_DIR}/packaging/release-gate.sh" \
    "${GATE_ARGS[@]}" --output "${ARTIFACT_DIR}/tampered-release.json"; then
    echo "release gate accepted cryptographically unbound raw benchmark" >&2
    exit 1
fi
grep -q 'benchmark: raw_sha256 does not match benchmark_raw' "${ARTIFACT_DIR}/tampered-release.json"

PREFLIGHT_ARTIFACT_DIR="${ARTIFACT_DIR}/preflight"
O3K_TESTLAB_ARTIFACT_DIR="${PREFLIGHT_ARTIFACT_DIR}" bash "${ROOT_DIR}/tests/testlab-libvirt.sh"
grep -q '"status": "skipped"' "${PREFLIGHT_ARTIFACT_DIR}/libvirt-result.json"

echo "release gate schema tests passed"
