#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-measure-auth.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT
BINARY="${ROOT_DIR}/target/debug/o3kd"
if [[ ! -x "${BINARY}" ]]; then
  cargo build --manifest-path "${ROOT_DIR}/Cargo.toml" --bin o3kd >/dev/null
fi

O3K_MEASURE_PROFILE=fake \
O3K_MEASURE_SAMPLES=1 \
O3K_BOOTSTRAP_PASSWORD='custom-"password' \
O3K_TOKEN_SIGNING_KEY='measurement-signing-key-with-at-least-32-bytes' \
O3K_MEASURE_BINARY="${BINARY}" \
O3K_MEASURE_ARTIFACT_DIR="${WORK_DIR}" \
bash "${ROOT_DIR}/tests/measure-testlab.sh"

python3 - "${WORK_DIR}/summary.json" <<'PY'
import json, sys

summary = json.load(open(sys.argv[1], encoding="utf-8"))
assert summary["status"] == "measured"
assert summary["control_plane"]["token_p95_seconds"] >= 0
print("measurement authentication test passed")
PY
