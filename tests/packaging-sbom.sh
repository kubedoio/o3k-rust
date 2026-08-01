#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-sbom.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

FAKE_CARGO="$WORK_DIR/cargo"
cat >"$FAKE_CARGO" <<'EOF'
#!/usr/bin/env bash
printf '{"packages": [], "workspace_members": []}\n'
EOF
chmod 0755 "$FAKE_CARGO"

DIST_TARGET="$WORK_DIR/dist-target"
DIST_LINK="$WORK_DIR/dist-link"
mkdir -p "$DIST_TARGET"
ln -s "$DIST_TARGET" "$DIST_LINK"
if O3K_RELEASE_DIST_DIR="$DIST_LINK" PATH="$WORK_DIR:$PATH" \
    bash "$ROOT_DIR/packaging/make-sbom.sh"; then
  echo "SBOM builder accepted a symlinked dist root" >&2
  exit 1
fi
[[ -z "$(find "$DIST_TARGET" -mindepth 1 -print -quit)" ]]

mkdir -p "$WORK_DIR/bin"
mv "$FAKE_CARGO" "$WORK_DIR/bin/cargo"
OUTPUT="$WORK_DIR/sbom.spdx.json"
PATH="$WORK_DIR/bin:$PATH" bash "$ROOT_DIR/packaging/make-sbom.sh" "$OUTPUT"
python3 - "$OUTPUT" <<'PY'
import json, sys
value = json.load(open(sys.argv[1], encoding="utf-8"))
assert value["spdxVersion"] == "SPDX-2.3"
assert value["packages"] == []
PY

echo "SBOM packaging tests passed"
