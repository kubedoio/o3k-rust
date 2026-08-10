#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)}"
PROFILE="${2:-fake}"
VERSION_RE='^[0-9]+(\.[0-9]+){1,2}(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$'
if [[ ! "$VERSION" =~ $VERSION_RE ]]; then
  echo "version must be a numeric release version with an optional prerelease suffix" >&2
  exit 2
fi
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }
if [[ -n "$(git -C "$ROOT_DIR" status --porcelain --untracked-files=all)" ]]; then
  echo "release source tree must be clean before packaging" >&2
  exit 2
fi
DIST_ROOT="${O3K_RELEASE_DIST_DIR:-$ROOT_DIR/dist}"
if [[ -L "$DIST_ROOT" || ( -e "$DIST_ROOT" && ! -d "$DIST_ROOT" ) ]]; then
  echo "release dist root must be a real directory, not a symlink or special file" >&2
  exit 2
fi
mkdir -p -- "$DIST_ROOT"
OUT_DIR="$DIST_ROOT/o3k-$VERSION"

# Release binaries must execute on every advertised target (Ubuntu 24.04 and
# Debian 12). They are built on the Debian 12 (bookworm, glibc 2.36) baseline
# with scripts/build-release-binaries-debian12.sh and handed in through
# O3K_RELEASE_BINARIES_DIR; building on a newer baseline (e.g. Ubuntu 24.04)
# produces binaries that fail at exec on Debian 12 and is rejected here by the
# glibc floor check.
BINARIES_DIR="${O3K_RELEASE_BINARIES_DIR:-}"
if [[ -n "$BINARIES_DIR" ]]; then
  [[ -f "$BINARIES_DIR/o3kd" ]] || { echo "baseline binary is missing: $BINARIES_DIR/o3kd" >&2; exit 2; }
  bash "$ROOT_DIR/packaging/check-glibc-baseline.sh" "$BINARIES_DIR/o3kd"
  if [[ "$PROFILE" == libvirt ]]; then
    [[ -f "$BINARIES_DIR/o3k-compute" ]] || { echo "baseline binary is missing: $BINARIES_DIR/o3k-compute" >&2; exit 2; }
    bash "$ROOT_DIR/packaging/check-glibc-baseline.sh" "$BINARIES_DIR/o3k-compute"
  fi
else
  cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd
  bash "$ROOT_DIR/packaging/check-glibc-baseline.sh" "$ROOT_DIR/target/release/o3kd"
  if [[ "$PROFILE" == libvirt ]]; then
    cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --features libvirt --bin o3k-compute-bin
    bash "$ROOT_DIR/packaging/check-glibc-baseline.sh" "$ROOT_DIR/target/release/o3k-compute-bin"
  fi
fi
rm -rf -- "$OUT_DIR"
mkdir -p "$OUT_DIR/bin" "$OUT_DIR/packaging" "$OUT_DIR/scripts" "$OUT_DIR/contracts" "$OUT_DIR/docs" "$OUT_DIR/examples"
if [[ -n "$BINARIES_DIR" ]]; then
  install -m 0755 "$BINARIES_DIR/o3kd" "$OUT_DIR/bin/o3kd"
  if [[ "$PROFILE" == libvirt ]]; then install -m 0755 "$BINARIES_DIR/o3k-compute" "$OUT_DIR/bin/o3k-compute"; fi
else
  install -m 0755 "$ROOT_DIR/target/release/o3kd" "$OUT_DIR/bin/o3kd"
  if [[ "$PROFILE" == libvirt ]]; then install -m 0755 "$ROOT_DIR/target/release/o3k-compute-bin" "$OUT_DIR/bin/o3k-compute"; fi
fi
cp "$ROOT_DIR/packaging/o3kd.service" "$ROOT_DIR/packaging/install.sh" "$ROOT_DIR/packaging/reset.sh" "$ROOT_DIR/packaging/uninstall.sh" "$ROOT_DIR/packaging/diagnose.sh" "$ROOT_DIR/packaging/preflight.sh" "$ROOT_DIR/packaging/bootstrap-certs.sh" "$ROOT_DIR/packaging/release-gate.sh" "$ROOT_DIR/packaging/validate-human-review.sh" "$ROOT_DIR/packaging/verify-release-bundle.sh" "$ROOT_DIR/packaging/check-glibc-baseline.sh" "$ROOT_DIR/packaging/o3k-compute.service" "$ROOT_DIR/packaging/50-o3k-libvirt.rules" "$OUT_DIR/packaging/"
cp "$ROOT_DIR/scripts/generate-passwords.sh" "$OUT_DIR/scripts/"
cp "$ROOT_DIR/scripts/validate-release-e2e-evidence.py" "$OUT_DIR/scripts/"
cp "$ROOT_DIR/contracts/release-e2e-evidence.schema.json" "$OUT_DIR/contracts/"
cp "$ROOT_DIR/docs/compatibility.md" "$ROOT_DIR/docs/cirros-walkthrough.md" "$ROOT_DIR/docs/release-evidence-schema.md" "$ROOT_DIR/docs/human-review-schema.md" "$ROOT_DIR/docs/security-review-checklist.md" "$ROOT_DIR/docs/releases/v0.2.0-alpha.1.md" "$OUT_DIR/docs/"
cp "$ROOT_DIR/examples/clouds.yaml" "$ROOT_DIR/examples/o3kd.env.example" "$OUT_DIR/examples/"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT_DIR" show -s --format=%ct HEAD)}" \
  "$ROOT_DIR/packaging/make-sbom.sh" "$OUT_DIR/sbom.spdx.json"
COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
WORKFLOW="${GITHUB_WORKFLOW:-local}"
printf '{"version":"%s","profile":"%s","source_commit":"%s","workflow":"%s"}\n' \
  "$VERSION" "$PROFILE" "$COMMIT" "$WORKFLOW" >"$OUT_DIR/manifest.json"
(cd "$OUT_DIR" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
bash "$ROOT_DIR/packaging/verify-release-bundle.sh" "$OUT_DIR"
echo "release prepared at $OUT_DIR"
