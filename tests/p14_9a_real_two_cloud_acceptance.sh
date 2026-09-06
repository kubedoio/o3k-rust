#!/usr/bin/env bash
# P14.9 prerequisite handoff. The Rust coordinator performs active probes;
# this wrapper does not treat environment variables or boolean flags as proof.
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT="${O3K_P14_9A_EVIDENCE_OUTPUT:-$ROOT_DIR/target/p14-9a/evidence.json}"
mkdir -p "$(dirname "$OUTPUT")"
[[ "$OUTPUT" = /* ]] || { echo "evidence output must be absolute" >&2; exit 2; }

export O3K_P14_TESTED_RUNTIME_HEAD_SHA="$(git -C "$ROOT_DIR" rev-parse HEAD)"
MODE="${O3K_P14_ACCEPTANCE_MODE:-prerequisites}"
O3K_P14_9A_EVIDENCE_OUTPUT="$OUTPUT" \
  cargo run --quiet --manifest-path "$ROOT_DIR/Cargo.toml" \
  -p o3k-migration --bin p14-acceptance -- "$MODE"

python3 "$ROOT_DIR/scripts/validate_p14_9a_evidence.py" "$OUTPUT" \
  --expected-head "$O3K_P14_TESTED_RUNTIME_HEAD_SHA"
result="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["execution_result"])' "$OUTPUT")"
printf 'P14.9 %s execution result: %s\n' "$MODE" "$result"
if [[ "$MODE" == "prerequisites" ]]; then
  [[ "$result" == "blocked" ]] && exit 2
else
  [[ "$result" == "passed" ]] || exit 2
fi
