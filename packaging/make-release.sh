#!/usr/bin/env bash
set -Eeuo pipefail
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VERSION="${1:-$(sed -n 's/^version = "\([^"]*\)"/\1/p' "$ROOT_DIR/Cargo.toml" | head -1)}"
OUT_DIR="$ROOT_DIR/dist/o3k-$VERSION"
cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd
rm -rf -- "$OUT_DIR"
mkdir -p "$OUT_DIR/bin" "$OUT_DIR/packaging"
install -m 0755 "$ROOT_DIR/target/release/o3kd" "$OUT_DIR/bin/o3kd"
cp "$ROOT_DIR/packaging/o3kd.service" "$ROOT_DIR/packaging/install.sh" "$ROOT_DIR/packaging/reset.sh" "$ROOT_DIR/packaging/uninstall.sh" "$OUT_DIR/packaging/"
(cd "$OUT_DIR" && sha256sum bin/o3kd > SHA256SUMS)
printf '{"version":"%s","profile":"fake","artifact":"bin/o3kd"}
' "$VERSION" >"$OUT_DIR/manifest.json"
echo "release prepared at $OUT_DIR"

