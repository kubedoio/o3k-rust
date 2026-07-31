#!/usr/bin/env bash
set -Eeuo pipefail

BUNDLE_DIR="${1:?usage: verify-release-bundle.sh BUNDLE_DIR}"
[[ -d "$BUNDLE_DIR" ]] || { echo "release bundle is not a directory: $BUNDLE_DIR" >&2; exit 2; }

python3 - "$BUNDLE_DIR" <<'PY'
import pathlib
import subprocess
import sys

bundle = pathlib.Path(sys.argv[1]).resolve()
checksums = bundle / "SHA256SUMS"
if not checksums.is_file() or checksums.is_symlink():
    raise SystemExit("release bundle must contain a regular SHA256SUMS file")

files = set()
for path in bundle.rglob("*"):
    relative = path.relative_to(bundle)
    if path.is_symlink():
        raise SystemExit(f"release bundle must not contain symlinks: {relative}")
    if path.is_file() and path != checksums:
        files.add(f"./{relative.as_posix()}")

entries = {}
for line_number, line in enumerate(checksums.read_text(encoding="utf-8").splitlines(), start=1):
    if not line or "  " not in line:
        raise SystemExit(f"SHA256SUMS line {line_number} is malformed")
    digest, name = line.split("  ", 1)
    if len(digest) != 64 or any(char not in "0123456789abcdef" for char in digest):
        raise SystemExit(f"SHA256SUMS line {line_number} has an invalid digest")
    if not name.startswith("./") or name in entries:
        raise SystemExit(f"SHA256SUMS line {line_number} has an invalid or duplicate path")
    candidate = pathlib.PurePosixPath(name)
    if candidate.is_absolute() or ".." in candidate.parts:
        raise SystemExit(f"SHA256SUMS line {line_number} escapes the bundle")
    entries[name] = digest

if set(entries) != files:
    missing = sorted(files - set(entries))
    extra = sorted(set(entries) - files)
    raise SystemExit(
        "SHA256SUMS must cover exactly the regular bundle files "
        f"(missing={missing}, extra={extra})"
    )

result = subprocess.run(
    ["sha256sum", "--check", "--strict", "SHA256SUMS"],
    cwd=bundle,
    check=False,
    text=True,
)
if result.returncode:
    raise SystemExit(result.returncode)
PY

echo "verified release bundle integrity: $BUNDLE_DIR"
