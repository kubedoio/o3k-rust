#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-evidence-manifest.XXXXXX")"
trap 'rm -rf -- "$WORK_DIR"' EXIT
for name in e2e ubuntu debian recovery benchmark benchmark-raw; do
  printf '{"source_commit":"%040d","o3kd_sha256":"%064d","o3k_compute_sha256":"%064d","bundle_tree_sha256":"%064d","finished_at":1,"status":"passed"}\n' 0 1 2 3 >"$WORK_DIR/$name.json"
done
ARGS=(
  --candidate-sha 0123456789abcdef0123456789abcdef01234567
  --o3kd-sha256 aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
  --o3k-compute-sha256 bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb
  --bundle-tree-sha256 cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc
  --artifact real_libvirt_e2e="$WORK_DIR/e2e.json"
  --artifact clean_ubuntu_install="$WORK_DIR/ubuntu.json"
  --artifact clean_debian_install="$WORK_DIR/debian.json"
  --artifact failure_recovery="$WORK_DIR/recovery.json"
  --artifact benchmark="$WORK_DIR/benchmark.json"
  --artifact benchmark_raw="$WORK_DIR/benchmark-raw.json"
  --output "$WORK_DIR/manifest.json"
)
# Replace the synthetic source field with the candidate SHA expected by the generator.
python3 - "$WORK_DIR" <<'PY'
import json, pathlib, sys
root = pathlib.Path(sys.argv[1])
for path in root.glob("*.json"):
    value = json.loads(path.read_text(encoding="utf-8"))
    value.update({
        "source_commit": "0123456789abcdef0123456789abcdef01234567",
        "o3kd_sha256": "a" * 64,
        "o3k_compute_sha256": "b" * 64,
        "bundle_tree_sha256": "c" * 64,
    })
    path.write_text(json.dumps(value), encoding="utf-8")
PY
python3 "$ROOT_DIR/packaging/generate-candidate-evidence-manifest.py" "${ARGS[@]}"
jq -e '.all_exact_candidate == true and .all_binary_bound == true and (.artifacts | length) == 6' "$WORK_DIR/manifest.json" >/dev/null

echo "candidate evidence manifest tests passed"
