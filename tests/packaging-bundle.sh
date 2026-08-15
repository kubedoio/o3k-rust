#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-bundle.XXXXXX")"
DIRTY_MARKER="$ROOT_DIR/.o3k-release-dirty-test"
ESCAPE_SENTINEL="$ROOT_DIR/o3k-release-escape-sentinel"
cleanup() { rm -f -- "$DIRTY_MARKER" "$ESCAPE_SENTINEL"; rm -rf -- "$WORK_DIR"; }
trap cleanup EXIT

BUNDLE_DIR="$WORK_DIR/bundle"
mkdir -p "$BUNDLE_DIR/bin" "$BUNDLE_DIR/packaging" "$BUNDLE_DIR/scripts"
for file in \
  install.sh o3kd.service reset.sh uninstall.sh diagnose.sh preflight.sh \
  bootstrap-certs.sh bootstrap-testlab.sh release-gate.sh validate-human-review.sh scan-release-evidence.sh generate-candidate-evidence-manifest.py o3k-compute.service 50-o3k-libvirt.rules; do
  cp "$ROOT_DIR/packaging/$file" "$BUNDLE_DIR/packaging/$file"
done
cp "$ROOT_DIR/scripts/generate-passwords.sh" "$BUNDLE_DIR/scripts/generate-passwords.sh"

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

printf 'must survive invalid version input\n' >"$ESCAPE_SENTINEL"
for invalid_version in '../../../o3k-release-escape' '1.2.3/escape' '1.2.3 bad' '1.2.3"quote'; do
  if CARGO_MARKER="$WORK_DIR/invalid-version-cargo" PATH="$CARGO_DIR:$PATH" \
      bash "$ROOT_DIR/packaging/make-release.sh" "$invalid_version" fake; then
    echo "release builder accepted unsafe version: $invalid_version" >&2
    exit 1
  fi
  [[ ! -e "$WORK_DIR/invalid-version-cargo" ]]
  grep -qx 'must survive invalid version input' "$ESCAPE_SENTINEL"
done
rm -f -- "$ESCAPE_SENTINEL"

DIST_TARGET="$WORK_DIR/dist-target"
DIST_LINK="$WORK_DIR/dist-link"
mkdir -p "$DIST_TARGET"
ln -s "$DIST_TARGET" "$DIST_LINK"
if O3K_RELEASE_DIST_DIR="$DIST_LINK" CARGO_MARKER="$WORK_DIR/symlink-dist-cargo" \
    PATH="$CARGO_DIR:$PATH" bash "$ROOT_DIR/packaging/make-release.sh" 0.0-symlink-dist fake; then
  echo "release builder accepted a symlinked dist root" >&2
  exit 1
fi
[[ ! -e "$WORK_DIR/symlink-dist-cargo" ]]
[[ -z "$(find "$DIST_TARGET" -mindepth 1 -print -quit)" ]]

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
if id o3k >/dev/null 2>&1 && id -nG o3k | tr ' ' '\n' | grep -Eq '^(libvirt|kvm)$'; then
  if CARGO_MARKER="$WORK_DIR/cargo-invoked" PATH="$CARGO_DIR:$PATH" bash "$BUNDLE_DIR/packaging/install.sh" \
      --profile libvirt --noninteractive \
      --prefix "$LIBVIRT_PREFIX" --data-dir "$WORK_DIR/libvirt-data" \
      --config-dir "$WORK_DIR/libvirt-config" --log-dir "$WORK_DIR/libvirt-log"; then
    echo "libvirt bundle installer reused a privileged control account" >&2
    exit 1
  fi
  echo "libvirt bundle install correctly refused a privileged control-account reuse"
else
  CARGO_MARKER="$WORK_DIR/cargo-invoked" PATH="$CARGO_DIR:$PATH" bash "$BUNDLE_DIR/packaging/install.sh" \
    --profile libvirt --noninteractive \
    --prefix "$LIBVIRT_PREFIX" --data-dir "$WORK_DIR/libvirt-data" \
    --config-dir "$WORK_DIR/libvirt-config" --log-dir "$WORK_DIR/libvirt-log"
  cmp -s "$BUNDLE_DIR/bin/o3kd" "$LIBVIRT_PREFIX/bin/o3kd"
  cmp -s "$BUNDLE_DIR/bin/o3k-compute" "$LIBVIRT_PREFIX/bin/o3k-compute"
  [[ -f "$LIBVIRT_PREFIX/share/o3k/o3k-compute.service" ]]
fi

echo "release bundle installer test passed"
