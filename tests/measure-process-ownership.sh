#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-measure-ownership.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

mkdir -p "$WORK_DIR/bin"
cat >"$WORK_DIR/bin/port-checker" <<'EOF'
#!/usr/bin/env bash
if [[ "${FAKE_PORT_STATE:-free}" == occupied ]]; then
  echo "fake listener owns $1:$2" >&2
  exit 1
fi
EOF
cat >"$WORK_DIR/bin/fake-o3kd" <<'EOF'
#!/usr/bin/env bash
if [[ "${FAKE_O3KD_STATE:-alive}" == exited ]]; then
  exit 42
fi
trap 'exit 0' TERM INT
while :; do sleep 1; done
EOF
cat >"$WORK_DIR/bin/curl" <<'EOF'
#!/usr/bin/env bash
if [[ "$*" == *'/readyz'* ]]; then
  exit 0
elif [[ "$*" == *'/v3/auth/tokens'* && "$*" == *'-fsSi'* ]]; then
  printf 'HTTP/1.1 201 Created\nX-Subject-Token: fake-token\n\n'
else
  printf '0.001'
fi
EOF
chmod 0755 "$WORK_DIR/bin/port-checker" "$WORK_DIR/bin/fake-o3kd" "$WORK_DIR/bin/curl"

run_measurement() {
  local state="$1"
  local artifacts="$WORK_DIR/$state"
  mkdir -p "$artifacts"
  set +e
  FAKE_PORT_STATE=free FAKE_O3KD_STATE="$state" \
    O3K_MEASURE_PROFILE=fake O3K_MEASURE_SAMPLES=1 \
    O3K_MEASURE_BINARY="$WORK_DIR/bin/fake-o3kd" \
    O3K_MEASURE_PORT=19152 O3K_MEASURE_PORT_CHECKER="$WORK_DIR/bin/port-checker" \
    O3K_MEASURE_ARTIFACT_DIR="$artifacts" PATH="$WORK_DIR/bin:$PATH" \
    bash "$ROOT_DIR/tests/measure-testlab.sh" >/dev/null
  local status=$?
  set -e
  printf '%s\n' "$status"
}

[[ "$(run_measurement alive)" == 0 ]]
python3 - "$WORK_DIR/alive/summary.json" <<'PY'
import json
import sys

summary = json.load(open(sys.argv[1], encoding="utf-8"))
assert summary["status"] == "measured"
PY

if [[ "$(run_measurement exited)" != 1 ]]; then
  echo "child exit was not rejected" >&2
  exit 1
fi
python3 - "$WORK_DIR/exited/diagnostic.json" <<'PY'
import json
import sys

diagnostic = json.load(open(sys.argv[1], encoding="utf-8"))
assert diagnostic["reason"] == "child_exited_during_readiness"
assert diagnostic["port"] == 19152
PY

OCCUPIED_DIR="$WORK_DIR/occupied"
mkdir -p "$OCCUPIED_DIR"
set +e
FAKE_PORT_STATE=occupied FAKE_O3KD_STATE=alive \
  O3K_MEASURE_PROFILE=fake O3K_MEASURE_SAMPLES=1 \
  O3K_MEASURE_BINARY="$WORK_DIR/bin/fake-o3kd" \
  O3K_MEASURE_PORT=19153 O3K_MEASURE_PORT_CHECKER="$WORK_DIR/bin/port-checker" \
  O3K_MEASURE_ARTIFACT_DIR="$OCCUPIED_DIR" PATH="$WORK_DIR/bin:$PATH" \
  bash "$ROOT_DIR/tests/measure-testlab.sh"
OCCUPIED_STATUS=$?
set -e
[[ "$OCCUPIED_STATUS" == 1 ]]
python3 - "$OCCUPIED_DIR/diagnostic.json" <<'PY'
import json
import sys

diagnostic = json.load(open(sys.argv[1], encoding="utf-8"))
assert diagnostic["reason"] == "port_occupied"
assert diagnostic["pid"] is None
assert diagnostic["port"] == 19153
PY

echo "measurement process ownership tests passed"
