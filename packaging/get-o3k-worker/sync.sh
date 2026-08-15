#!/usr/bin/env bash
set -Eeuo pipefail

# sync.sh — regenerate the embedded assets of the get.o3k.io Cloudflare Worker
# from the repo's single sources of truth, and prove the committed artifact is
# not stale.
#
#   bash packaging/get-o3k-worker/sync.sh          write src/assets.js
#   bash packaging/get-o3k-worker/sync.sh --check  fail (exit 1) when the
#                                                  committed src/assets.js does
#                                                  not match the sources
#
# Single source of truth (do not edit src/assets.js by hand):
#   packaging/get-o3k.sh     served verbatim at GET / and GET /install.sh
#   packaging/channels.yaml  channel table served at GET /channel/<name>
#
# The generated file IS committed: it is the deployable worker artifact, and
# CI runs sync.sh --check so drift between the sources and the deployable is
# caught in review instead of at publish time.
WORKER_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT_DIR="$(cd "$WORKER_DIR/../.." && pwd)"
OUT="$WORKER_DIR/src/assets.js"

TMP_FILE="$(mktemp "${TMPDIR:-/tmp}/o3k-worker-assets.XXXXXX")"
trap 'rm -f -- "$TMP_FILE"' EXIT

python3 - "$ROOT_DIR" >"$TMP_FILE" <<'PY'
import json
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])

script = (root / "packaging" / "get-o3k.sh").read_text(encoding="utf-8")
if not script.startswith("#!/usr/bin/env bash\n"):
    raise SystemExit("packaging/get-o3k.sh does not start with the expected shebang")

VERSION_RE = re.compile(
    r"^[0-9]+(\.[0-9]+){1,2}(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$"
)
CHANNEL_LINE = re.compile(r"^  ([A-Za-z0-9_-]+): (\S+)$")

# channels.yaml is a fixed O3K-controlled format; parse it strictly so a
# drifted shape fails generation instead of silently producing a wrong table.
channels = {}
in_channels = False
for number, line in enumerate(
    (root / "packaging" / "channels.yaml").read_text(encoding="utf-8").splitlines(),
    start=1,
):
    stripped = line.strip()
    if not stripped or stripped.startswith("#"):
        continue
    if stripped == "channels:":
        if in_channels:
            raise SystemExit(f"channels.yaml line {number}: duplicate channels: key")
        in_channels = True
        continue
    if not in_channels:
        raise SystemExit(f"channels.yaml line {number}: content outside channels: block")
    match = CHANNEL_LINE.match(line)
    if not match:
        raise SystemExit(
            f"channels.yaml line {number}: expected '  <name>: <version>', got: {line!r}"
        )
    name, version = match.groups()
    if name in channels:
        raise SystemExit(f"channels.yaml line {number}: duplicate channel {name}")
    if not VERSION_RE.match(version.lstrip("v")):
        raise SystemExit(
            f"channels.yaml line {number}: channel {name} maps to an "
            f"invalid release version: {version}"
        )
    channels[name] = version

if not channels:
    raise SystemExit("channels.yaml declares no channels")
if "alpha" not in channels:
    raise SystemExit(
        "channels.yaml must declare the alpha channel (packaging/get-o3k.sh "
        "resolves it when no version is pinned)"
    )

header = (
    "// GENERATED FILE — DO NOT EDIT. Regenerate with:\n"
    "//   bash packaging/get-o3k-worker/sync.sh\n"
    "// Embedded snapshots of the installer endpoint assets (single sources of\n"
    "// truth are packaging/get-o3k.sh and packaging/channels.yaml). CI runs\n"
    "// sync.sh --check to fail stale snapshots.\n"
)
print(header, end="")
print(f"export const SCRIPT = {json.dumps(script, ensure_ascii=True)};")
print(f"export const CHANNELS = Object.freeze({json.dumps(channels, ensure_ascii=True)});")
print("export const ALPHA_TARGET = CHANNELS.alpha;")
PY

if [[ "${1:-}" == "--check" ]]; then
  if ! cmp -s -- "$TMP_FILE" "$OUT"; then
    echo "get.o3k.io worker assets are stale: $OUT" >&2
    echo "run: bash packaging/get-o3k-worker/sync.sh" >&2
    diff -u -- "$OUT" "$TMP_FILE" >&2 || true
    exit 1
  fi
  echo "get.o3k.io worker assets in sync with packaging/get-o3k.sh and packaging/channels.yaml"
  exit 0
fi
[[ $# -eq 0 ]] || { echo "usage: sync.sh [--check]" >&2; exit 2; }
mv -f -- "$TMP_FILE" "$OUT"
echo "wrote $OUT"
