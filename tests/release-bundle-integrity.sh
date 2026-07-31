#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-release-bundle-integrity.XXXXXX")"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

BUNDLE_DIR="$WORK_DIR/bundle"
mkdir -p "$BUNDLE_DIR/bin" "$BUNDLE_DIR/docs"
printf 'binary\n' >"$BUNDLE_DIR/bin/o3kd"
printf 'documentation\n' >"$BUNDLE_DIR/docs/README"
(cd "$BUNDLE_DIR" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum >SHA256SUMS)

bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"

printf 'tampered\n' >>"$BUNDLE_DIR/bin/o3kd"
if bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"; then
  echo "bundle verifier accepted a modified file" >&2
  exit 1
fi

printf 'binary\n' >"$BUNDLE_DIR/bin/o3kd"
printf 'unlisted\n' >"$BUNDLE_DIR/docs/UNLISTED"
if bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"; then
  echo "bundle verifier accepted an unlisted file" >&2
  exit 1
fi

rm "$BUNDLE_DIR/docs/UNLISTED"
ln -s o3kd "$BUNDLE_DIR/bin/linked-o3kd"
if bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR"; then
  echo "bundle verifier accepted a symlink" >&2
  exit 1
fi

echo "release bundle integrity tests passed"
