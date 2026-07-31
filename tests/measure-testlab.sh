#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PROFILE="${O3K_MEASURE_PROFILE:-fake}"
SAMPLES="${O3K_MEASURE_SAMPLES:-5}"
ARTIFACT_DIR="${O3K_MEASURE_ARTIFACT_DIR:-${ROOT_DIR}/target/measurements}"
BINARY="${O3K_MEASURE_BINARY:-}"
DATA_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-measure.XXXXXX")"
PORT="${O3K_MEASURE_PORT:-$((19080 + ($$ % 500)))}"
BASE_URL="http://127.0.0.1:${PORT}"
LISTEN_ADDR="127.0.0.1:${PORT}"
mkdir -p "$ARTIFACT_DIR"
rm -f "$ARTIFACT_DIR/raw.json" "$ARTIFACT_DIR/summary.json" "$ARTIFACT_DIR/o3kd.log"
O3KD_PID=
cleanup() {
  local exit_code="$?"
  local cleanup_status=passed
  set +e
  if [[ -n "${O3KD_PID}" ]]; then
    if kill -TERM "${O3KD_PID}" 2>/dev/null; then
      wait "${O3KD_PID}" 2>/dev/null || true
    elif kill -0 "${O3KD_PID}" 2>/dev/null; then
      cleanup_status=failed
    fi
  fi
  rm -rf -- "$DATA_DIR" || cleanup_status=failed
  if [[ -f "$ARTIFACT_DIR/summary.json" ]]; then
    python3 - "$ARTIFACT_DIR/summary.json" "$cleanup_status" <<'PY'
import json, sys
path, cleanup_status = sys.argv[1:]
with open(path, encoding="utf-8") as stream:
    result = json.load(stream)
if result.get("status") == "measured":
    result["cleanup"] = {"status": cleanup_status}
    with open(path, "w", encoding="utf-8") as stream:
        json.dump(result, stream, indent=2, sort_keys=True)
        stream.write("\n")
PY
  fi
  exit "$exit_code"
}
trap cleanup EXIT

if [[ "$PROFILE" == libvirt ]] && { [[ ! -e /dev/kvm ]] || ! command -v virsh >/dev/null 2>&1; }; then
  python3 - "$ARTIFACT_DIR/summary.json" <<'PY'
import json, sys
with open(sys.argv[1], "w", encoding="utf-8") as output:
    json.dump({
        "artifact_type": "benchmark", "status": "skipped", "profile": "libvirt",
        "reason": "virsh or /dev/kvm unavailable", "redacted": True,
        "cleanup": {"status": "not_run"}, "finished_at": int(__import__("time").time()),
    }, output, indent=2)
    output.write("\n")
PY
  echo "measurement skipped: real libvirt prerequisites unavailable" >&2
  exit 0
fi
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }
[[ "$SAMPLES" =~ ^[1-9][0-9]*$ ]] || { echo "O3K_MEASURE_SAMPLES must be positive" >&2; exit 2; }

if [[ -z "$BINARY" ]]; then
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd >/dev/null
  BINARY="$ROOT_DIR/target/release/o3kd"
fi
[[ -x "$BINARY" ]] || { echo "measurement binary is not executable" >&2; exit 1; }
PASSWORD="${O3K_BOOTSTRAP_PASSWORD:-measurement-password}"
SIGNING_KEY="${O3K_TOKEN_SIGNING_KEY:-measurement-signing-key-with-at-least-32-bytes}"
START_NS="$(date +%s%N)"
O3K_BOOTSTRAP_PASSWORD="$PASSWORD" O3K_TOKEN_SIGNING_KEY="$SIGNING_KEY" "$BINARY" --listen-addr "$LISTEN_ADDR" --data-dir "$DATA_DIR" --log-filter warn >"$ARTIFACT_DIR/o3kd.log" 2>&1 &
O3KD_PID=$!
READY_NS=
for _ in $(seq 1 200); do
  if curl -fsS "$BASE_URL/readyz" >/dev/null 2>&1; then READY_NS="$(date +%s%N)"; break; fi
  sleep 0.01
done
[[ -n "$READY_NS" ]] || { echo "o3kd did not become ready" >&2; exit 1; }
AUTH_BODY="$(MEASURE_PASSWORD="$PASSWORD" python3 - <<'PY'
import json
import os

print(json.dumps({
    "auth": {
        "identity": {
            "methods": ["password"],
            "password": {"user": {"name": "admin", "password": os.environ["MEASURE_PASSWORD"]}},
        },
        "scope": {"project": {"name": "admin"}},
    }
}))
PY
)"
TOKEN_HEADERS="$(curl -fsSi -X POST "$BASE_URL/v3/auth/tokens" -H 'content-type: application/json' --data "$AUTH_BODY")"
TOKEN="$(python3 -c 'import sys; print(next((line.split(":",1)[1].strip() for line in sys.stdin.read().splitlines() if line.lower().startswith("x-subject-token:")), ""))' <<<"$TOKEN_HEADERS")"
[[ -n "$TOKEN" ]] || { echo "token issue failed" >&2; exit 1; }
TOKEN_TIMES=
for _ in $(seq 1 "$SAMPLES"); do
  TOKEN_TIMES+="$(curl -fsS -o /dev/null -w '%{time_total}' -X POST "$BASE_URL/v3/auth/tokens" -H 'content-type: application/json' --data "$AUTH_BODY") "
done
READY_MS="$(( (READY_NS - START_NS) / 1000000 ))"
RSS_KIB="$(awk '/VmRSS:/ {print $2}' "/proc/$O3KD_PID/status" 2>/dev/null || echo null)"
BINARY_BYTES="$(stat -c %s "$BINARY")"
export ARTIFACT_DIR PROFILE SAMPLES READY_MS RSS_KIB BINARY_BYTES TOKEN_TIMES
python3 <<'PY'
import json, math, os, platform, subprocess
times = [float(value) for value in os.environ["TOKEN_TIMES"].split()]
ordered = sorted(times)
p95 = ordered[min(len(ordered) - 1, max(0, math.ceil(len(ordered) * 0.95) - 1))]
raw = {
    "artifact_type": "benchmark", "status": "measured", "profile": os.environ["PROFILE"], "samples": int(os.environ["SAMPLES"]), "redacted": True,
    "environment": {"uname": platform.platform(), "rustc": subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()},
    "control_plane": {"startup_readiness_ms": int(os.environ["READY_MS"]), "token_seconds": times, "token_p95_seconds": p95, "idle_rss_kib": None if os.environ["RSS_KIB"] == "null" else int(os.environ["RSS_KIB"]), "o3kd_binary_bytes": int(os.environ["BINARY_BYTES"])},
    "guest_and_libvirt": {"status": "not_measured", "reason": "control-plane harness does not claim real guest coverage"},
    "targets": {"startup_readiness_ms": 2000, "idle_rss_mib": 150, "token_p95_ms": 100},
}
with open(os.path.join(os.environ["ARTIFACT_DIR"], "raw.json"), "w", encoding="utf-8") as output:
    json.dump(raw, output, indent=2, sort_keys=True); output.write("\n")
control = raw["control_plane"]
summary = {"artifact_type": "benchmark", "status": "measured", "profile": raw["profile"], "samples": raw["samples"], "redacted": True, "finished_at": int(__import__("time").time()), "control_plane": control, "guest_and_libvirt": raw["guest_and_libvirt"], "cleanup": {"status": "pending"}, "targets_evaluated": {"startup": control["startup_readiness_ms"] <= 2000, "rss": control["idle_rss_kib"] is not None and control["idle_rss_kib"] <= 150 * 1024, "token_p95": p95 <= 0.1}, "note": "Thresholds are evaluations, not production guarantees; no OpenStack comparison is made."}
with open(os.path.join(os.environ["ARTIFACT_DIR"], "summary.json"), "w", encoding="utf-8") as output:
    json.dump(summary, output, indent=2, sort_keys=True); output.write("\n")
PY
echo "measurement complete: $ARTIFACT_DIR/raw.json and summary.json"
