#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-bundle.XXXXXX")"
DIRTY_MARKER="$ROOT_DIR/.o3k-release-dirty-test"
cleanup() { rm -f -- "$DIRTY_MARKER"; rm -rf -- "$WORK_DIR"; }
trap cleanup EXIT

BUNDLE_DIR="$WORK_DIR/bundle"
mkdir -p "$BUNDLE_DIR/bin" "$BUNDLE_DIR/packaging"
for file in \
  install.sh o3kd.service reset.sh uninstall.sh diagnose.sh preflight.sh \
  bootstrap-certs.sh release-gate.sh validate-human-review.sh o3k-compute.service; do
  cp "$ROOT_DIR/packaging/$file" "$BUNDLE_DIR/packaging/$file"
done

printf '#!/usr/bin/env bash\nprintf bundled-o3kd\n' >"$BUNDLE_DIR/bin/o3kd"
chmod 0755 "$BUNDLE_DIR/bin/o3kd"
printf '#!/usr/bin/env bash\nprintf bundled-o3k-compute\n' >"$BUNDLE_DIR/bin/o3k-compute"
chmod 0755 "$BUNDLE_DIR/bin/o3k-compute"

CARGO_DIR="$WORK_DIR/no-cargo"
mkdir -p "$CARGO_DIR"
cat >"$CARGO_DIR/cargo" <<'EOF'
#!/usr/bin/env bash
touch "${CARGO_MARKER:?}"
printf 'cargo must not be called for a release bundle\n' >&2
exit 97
EOF
chmod 0755 "$CARGO_DIR/cargo"

touch "$DIRTY_MARKER"
if CARGO_MARKER="$WORK_DIR/dirty-cargo-invoked" PATH="$CARGO_DIR:$PATH" \
    bash "$ROOT_DIR/packaging/make-release.sh" 0.0-dirty-test fake; then
  echo "release builder accepted a dirty source tree" >&2
  exit 1
fi
[[ ! -e "$WORK_DIR/dirty-cargo-invoked" ]]
rm -f -- "$DIRTY_MARKER"

PREFIX="$WORK_DIR/prefix"
CARGO_MARKER="$WORK_DIR/cargo-invoked" PATH="$CARGO_DIR:$PATH" bash "$BUNDLE_DIR/packaging/install.sh" \
  --profile fake --noninteractive \
  --prefix "$PREFIX" --data-dir "$WORK_DIR/data" \
  --config-dir "$WORK_DIR/config" --log-dir "$WORK_DIR/log"

cmp -s "$BUNDLE_DIR/bin/o3kd" "$PREFIX/bin/o3kd"
[[ -f "$PREFIX/share/o3k/o3kd.service" ]]
[[ ! -e "$WORK_DIR/cargo-invoked" ]]

# Keep this deterministic and host-independent: the real preflight contract is
# exercised separately, while this invocation only verifies bundle selection.
cat >"$BUNDLE_DIR/packaging/preflight.sh" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
EOF
chmod 0755 "$BUNDLE_DIR/packaging/preflight.sh"

# A libvirt bundle must validate certificate inputs before creating any
# installation state. This keeps a failed clean install repair-free.
if bash "$BUNDLE_DIR/packaging/install.sh" \
    --profile libvirt --noninteractive \
    --prefix "$WORK_DIR/invalid-libvirt-prefix" \
    --data-dir "$WORK_DIR/invalid-libvirt-data" \
    --config-dir "$WORK_DIR/invalid-libvirt-config" \
    --log-dir "$WORK_DIR/invalid-libvirt-log"; then
  echo "libvirt installer accepted missing TLS inputs" >&2
  exit 1
fi
[[ ! -e "$WORK_DIR/invalid-libvirt-prefix" && ! -e "$WORK_DIR/invalid-libvirt-data" ]]

TLS_DIR="$WORK_DIR/libvirt-config/tls"
mkdir -p "$TLS_DIR"
printf 'o3k-owned-v1 path=%s\n' "$WORK_DIR/libvirt-config" >"$WORK_DIR/libvirt-config/.o3k-owned"
for file in ca.pem server.pem server-key.pem agent.pem agent-key.pem; do
  printf 'test credential\n' >"$TLS_DIR/$file"
done
printf '%064d\n' 0 >"$TLS_DIR/agent-fingerprint"
printf 'compute-agent\n' >"$TLS_DIR/agent-id"

LIBVIRT_PREFIX="$WORK_DIR/libvirt-prefix"
CARGO_MARKER="$WORK_DIR/cargo-invoked" PATH="$CARGO_DIR:$PATH" bash "$BUNDLE_DIR/packaging/install.sh" \
  --profile libvirt --noninteractive \
  --prefix "$LIBVIRT_PREFIX" --data-dir "$WORK_DIR/libvirt-data" \
  --config-dir "$WORK_DIR/libvirt-config" --log-dir "$WORK_DIR/libvirt-log"

cmp -s "$BUNDLE_DIR/bin/o3kd" "$LIBVIRT_PREFIX/bin/o3kd"
cmp -s "$BUNDLE_DIR/bin/o3k-compute" "$LIBVIRT_PREFIX/bin/o3k-compute"
[[ -f "$LIBVIRT_PREFIX/share/o3k/o3k-compute.service" ]]

echo "release bundle installer test passed"
