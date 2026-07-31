#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)}"
PROFILE="${2:-fake}"
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }
OUT_DIR="$ROOT_DIR/dist/o3k-$VERSION"
cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd
if [[ "$PROFILE" == libvirt ]]; then cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --features libvirt --bin o3k-compute; fi
rm -rf -- "$OUT_DIR"
mkdir -p "$OUT_DIR/bin" "$OUT_DIR/packaging" "$OUT_DIR/docs" "$OUT_DIR/examples"
install -m 0755 "$ROOT_DIR/target/release/o3kd" "$OUT_DIR/bin/o3kd"
if [[ "$PROFILE" == libvirt ]]; then install -m 0755 "$ROOT_DIR/target/release/o3k-compute" "$OUT_DIR/bin/o3k-compute"; fi
cp "$ROOT_DIR/packaging/o3kd.service" "$ROOT_DIR/packaging/install.sh" "$ROOT_DIR/packaging/reset.sh" "$ROOT_DIR/packaging/uninstall.sh" "$ROOT_DIR/packaging/diagnose.sh" "$ROOT_DIR/packaging/preflight.sh" "$ROOT_DIR/packaging/bootstrap-certs.sh" "$ROOT_DIR/packaging/release-gate.sh" "$ROOT_DIR/packaging/validate-human-review.sh" "$ROOT_DIR/packaging/o3k-compute.service" "$OUT_DIR/packaging/"
cp "$ROOT_DIR/docs/compatibility.md" "$ROOT_DIR/docs/cirros-walkthrough.md" "$ROOT_DIR/docs/release-evidence-schema.md" "$ROOT_DIR/docs/human-review-schema.md" "$ROOT_DIR/docs/security-review-checklist.md" "$ROOT_DIR/docs/releases/v0.2.0-alpha.1.md" "$OUT_DIR/docs/"
cp "$ROOT_DIR/examples/clouds.yaml" "$ROOT_DIR/examples/o3kd.env.example" "$OUT_DIR/examples/"
SOURCE_DATE_EPOCH="${SOURCE_DATE_EPOCH:-$(git -C "$ROOT_DIR" show -s --format=%ct HEAD)}" \
  "$ROOT_DIR/packaging/make-sbom.sh" "$OUT_DIR/sbom.spdx.json"
COMMIT="$(git -C "$ROOT_DIR" rev-parse HEAD)"
WORKFLOW="${GITHUB_WORKFLOW:-local}"
printf '{"version":"%s","profile":"%s","source_commit":"%s","workflow":"%s"}\n' \
  "$VERSION" "$PROFILE" "$COMMIT" "$WORKFLOW" >"$OUT_DIR/manifest.json"
(cd "$OUT_DIR" && find . -type f ! -name SHA256SUMS -print0 | sort -z | xargs -0 sha256sum > SHA256SUMS)
echo "release prepared at $OUT_DIR"
