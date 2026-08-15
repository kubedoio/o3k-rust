#!/usr/bin/env bash
# get-o3k.sh — thin one-line installer wrapper (issue #613).
#
# Published as the GitHub Release asset install.sh of every O3K release: the
# release generator exports this file byte-for-byte as dist/install.sh
# (packaging/make-release.sh, 0755, drift-gated by cmp), so the canonical
# alpha invocation is
#   curl -sfL https://github.com/kubedoio/o3k-rust/releases/download/v0.2.0-alpha.2/install.sh | sudo sh -
# get.o3k.io is only a convenience 302 redirect to that exact asset:
#   curl -sfL https://get.o3k.io | sudo sh -
#
# This file is POSIX-sh compatible on purpose: on Ubuntu 24.04 and Debian 12
# `sudo sh -` is dash, so the piped invocation must not depend on bashisms.
# Fail-fast is `set -eu` plus pipefail where the shell supports it — Ubuntu
# 24.04's dash (0.5.12-6ubuntu5) rejects `set -o pipefail`, so it is enabled
# conditionally (empirically verified on a clean noble VM).
#
# SECURITY CONTRACT — no curl|sh of unverified content:
#   The version to install is BAKED into this file (O3K_INSTALLER_VERSION);
#   the installer never consults a channel service or any other network
#   endpoint to decide which version to install. Every file that is executed
#   (packaging/*.sh, bin/o3kd, bin/o3k-compute) comes from the release
#   tarball AFTER its published SHA-256 is verified; the tarball is never
#   extracted before that verification, and extraction rejects any entry that
#   is absolute, contains a ".." component, does not start with "./", or is
#   not a regular file (symlink, hardlink, device, fifo, socket entries are
#   refused from the `tar -tvzf` listing before anything is written). Any
#   download or verification failure aborts.
#
# An optional endpoint may serve this same file with an added FIRST line
#   O3K_PINNED_VERSION="v0.2.0-alpha.2"
# which is a plain shell assignment when the stream is piped to sh. It is
# kept for optional future /v<version> endpoint paths (packaging/get-o3k-worker)
# and handled by the resolution order below; its absence is not an error.
# There is no fallback to main/latest: resolution failure aborts.
#
# Version resolution precedence:
#   1. O3K_VERSION environment variable (explicit dev/test override);
#   2. O3K_PINNED_VERSION (the endpoint-injected first line, if present);
#   3. the baked O3K_INSTALLER_VERSION release pin (this file's own release).
# The installer NEVER consults a channel service.
#
# Supported platforms (strict): Linux x86_64, Ubuntu 24.04 (noble) or
# Debian 12 (bookworm). Anything else fails with a clear message. Root is
# required.
#
# Overrides for testing/campaigns (the only knobs):
#   O3K_RELEASE_BASE  release asset base (default
#                     https://github.com/kubedoio/o3k-rust/releases/download)
# Local campaigns serve the release assets from one http server:
# /releases/v<version>/<assets>; HTTP is permitted only for this explicit
# override, production URLs are pinned to HTTPS.
#
# Release asset naming contract (packaging/make-release-archive.sh):
#   o3k-<version>-linux-x86_64.tar.gz + o3k-<version>-linux-x86_64.tar.gz.sha256
#   at <release-base>/v<version>/.
#
# The dependency package list mirrors the exact host set proven by the clean
# Ubuntu 24.04 and Debian 12 VM campaigns
# (target/real-host-workflow-artifacts/asr-021-cd15263/vm-run.sh cloud-init
# package list plus openssl/openssh-client for the bundled bootstrap scripts
# and binutils for readelf in the bundled verify-release-bundle.sh glibc floor
# check).
# This is the ONE place the outer installer may apt-install. It does NOT touch
# netplan, systemd-networkd, sysctl forwarding, or host-wide NAT (goal §11).
set -eu
if (set -o pipefail) 2>/dev/null; then
  set -o pipefail
fi

# Baked release pin — updated in EVERY release's version-bump commit; the
# published install.sh GitHub Release asset is byte-identical to this file,
# so an installer downloaded from .../releases/download/v<version>/install.sh
# installs exactly <version> by default.
O3K_INSTALLER_VERSION="v0.2.0-alpha.2"
O3K_RELEASE_BASE="${O3K_RELEASE_BASE:-https://github.com/kubedoio/o3k-rust/releases/download}"
INSTALL_MANIFEST=/usr/local/share/o3k/.o3k-installed

die() { printf 'O3K installer: %s\n' "$1" >&2; exit 1; }
step() { printf '✓ %s\n' "$1"; }

# Platform guard — self-contained so the test matrix can exercise it with
# faked inputs in a subshell.
check_platform() {
  local kernel="$1" machine="$2" distro_id="$3" codename="$4"
  [ "$kernel" = Linux ] || {
    printf 'unsupported kernel: %s — only Linux x86_64 is supported\n' "$kernel" >&2
    exit 1
  }
  [ "$machine" = x86_64 ] || {
    printf 'unsupported architecture: %s — only x86_64 is supported\n' "$machine" >&2
    exit 1
  }
  case "$distro_id:$codename" in
    ubuntu:noble|debian:bookworm) ;;
    *)
      printf 'unsupported distribution: %s %s — only Ubuntu 24.04 (noble) and Debian 12 (bookworm) are supported\n' \
        "${distro_id:-unknown}" "${codename:-unknown}" >&2
      exit 1
      ;;
  esac
}

# Version fence — the ADR-0130 release-version format
# (docs/adr/ADR-0130-release-version-path-fence.md): numeric release version
# with one or two dots and an optional dot-separated alphanumeric prerelease
# suffix, nothing else; an optional leading "v" is accepted and stripped.
# Self-contained so the test matrix can exercise it with faked inputs.
check_version_format() {
  local version="${1#v}"
  printf '%s\n' "$version" \
    | grep -Eq '^[0-9]+(\.[0-9]+){1,2}(-[0-9A-Za-z]+(\.[0-9A-Za-z]+)*)?$' \
    || {
      printf 'unsupported version: %s — expected a published release version like v0.2.0-alpha.2; refusing to fall back to main/latest\n' "$1" >&2
      exit 1
    }
}

trim() { # dash-safe: dash does not implement [:space:] in expansion patterns
  printf '%s' "$1" | sed -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//'
}

fetch() { # fetch URL DESTINATION — fail closed, no silent retry exhaustion
  local url="$1" destination="$2" proto='=https'
  case "$url" in
    http://*) proto='=http,https' ;; # only for the documented local overrides
  esac
  curl --fail --location --retry 3 --retry-all-errors --connect-timeout 15 \
    --max-time 600 --proto "$proto" --output "$destination" "$url" \
    || die "download failed: $url"
}

safe_extract() { # safe_extract TARBALL DESTINATION ENTRIES_FILE
  local tarball="$1" destination="$2" entries="$3" entry
  if ! tar -tzf "$tarball" >"$entries" 2>/dev/null; then
    die "release archive is not a readable gzip tarball: $tarball"
  fi
  while IFS= read -r entry; do
    [ -n "$entry" ] || die "release archive contains an empty entry"
    case "$entry" in
      ./*) ;;
      *) die "unsafe release archive entry (must start with ./): $entry" ;;
    esac
    case "$entry" in
      *'/../'*|*'/..'|'..') die "unsafe release archive entry (.. component): $entry" ;;
    esac
  done <"$entries"
  # Second pass over the verbose listing: refuse any non-regular entry type
  # (symlink, device, fifo, socket) before extraction so an archive cannot
  # plant a link or special file in the extraction tree. The leading listing
  # character is the entry type; ` -> ` is the GNU tar symlink target marker.
  # Checking both is deliberate: a tar implementation that renders a link
  # differently is still caught by the type character.
  if ! tar -tvzf "$tarball" >"$entries.types" 2>/dev/null; then
    die "release archive is not a readable gzip tarball: $tarball"
  fi
  while IFS= read -r entry; do
    case "$entry" in
      *' -> '*) die "unsafe release archive entry (symlink): $entry" ;;
    esac
    case "$(printf '%s' "$entry" | cut -c1)" in
      l|b|c|p|s) die "unsafe release archive entry (device, fifo, or link): $entry" ;;
    esac
  done <"$entries.types"
  tar -xzf "$tarball" -C "$destination" \
    || die "release archive extraction failed: $tarball"
}

wait_http_ok() { # wait_http_ok URL ATTEMPTS
  local url="$1" attempts="$2" attempt=1
  while [ "$attempt" -le "$attempts" ]; do
    if curl --fail --silent --output /dev/null --max-time 5 "$url"; then
      return 0
    fi
    sleep 2
    attempt=$((attempt + 1))
  done
  return 1
}

# Idempotency notice — self-contained so the test matrix can exercise it with
# a faked manifest path in a subshell.
print_installed_notice() {
  if [ -f "$INSTALL_MANIFEST" ] && [ ! -L "$INSTALL_MANIFEST" ]; then
    printf 'O3K v%s already installed\n' "${VERSION_NO_V:-}"
  fi
}

# ---- platform and privilege guards -------------------------------------------
if [ "$(id -u)" -ne 0 ]; then
  printf 'O3K installer: root is required; run: curl -sfL https://get.o3k.io | sudo sh -\n' >&2
  exit 1
fi
# shellcheck disable=SC1091
. /etc/os-release 2>/dev/null || true
check_platform "$(uname -s)" "$(uname -m)" "${ID:-}" "${VERSION_CODENAME:-}"

printf 'O3K Cloud OS — TestLab\n'
printf '✓ %s %s\n' "${PRETTY_NAME:-${ID:-unknown} ${VERSION_ID:-unknown}}" "$(uname -m)"

# ---- private temp dir + cleanup ----------------------------------------------
TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-installer.XXXXXX")"
chmod 700 "$TMP_DIR"
trap 'rm -rf -- "$TMP_DIR"' EXIT
trap 'rm -rf -- "$TMP_DIR"; exit 130' INT
trap 'rm -rf -- "$TMP_DIR"; exit 143' TERM HUP

# ---- version resolution -------------------------------------------------------
# The version is baked into this file; the installer never consults a channel
# service. Explicit dev/test overrides take precedence.
VERSION="${O3K_VERSION:-${O3K_PINNED_VERSION:-$O3K_INSTALLER_VERSION}}"
VERSION="$(trim "$VERSION")"
[ -n "$VERSION" ] || die "no installer version resolved"
check_version_format "$VERSION"
VERSION_NO_V="${VERSION#v}"
print_installed_notice

# ---- host dependencies (the one place apt is allowed) -------------------------
# Mirror the proven clean-VM set (asr-021-cd15263/vm-run.sh cloud-init):
# ca-certificates curl libvirt-daemon-system libvirt-clients qemu-utils
# iproute2 dnsmasq-base polkitd genisoimage python3 python3-openstackclient,
# with the distro-specific QEMU package (Ubuntu 24.04: qemu-kvm; Debian 12:
# qemu-system-x86), plus openssl (bootstrap-certs.sh) and openssh-client
# (ssh-keygen in bootstrap-testlab.sh), plus binutils (readelf for the bundled
# verify-release-bundle.sh glibc floor check). libvirtd is enabled/started
# exactly like the proven VM setup did. No netplan/NAT/sysctl/host-network
# writes.
printf 'installing host dependencies (apt)\n'
APT_PACKAGES="ca-certificates curl openssl openssh-client binutils libvirt-daemon-system libvirt-clients qemu-utils iproute2 dnsmasq-base polkitd genisoimage python3 python3-openstackclient"
case "$ID:$VERSION_CODENAME" in
  ubuntu:noble) APT_PACKAGES="$APT_PACKAGES qemu-kvm" ;;
  debian:bookworm) APT_PACKAGES="$APT_PACKAGES qemu-system-x86" ;;
esac
export DEBIAN_FRONTEND=noninteractive
apt-get update
# shellcheck disable=SC2086
apt-get install -y $APT_PACKAGES
systemctl enable --now libvirtd
step 'dependencies ready'

# ---- download the certified release bundle ------------------------------------
ASSET="o3k-${VERSION_NO_V}-linux-x86_64.tar.gz"
ASSET_URL="$O3K_RELEASE_BASE/v$VERSION_NO_V/$ASSET"
printf 'downloading %s\n' "$ASSET"
fetch "$ASSET_URL" "$TMP_DIR/$ASSET"
fetch "$ASSET_URL.sha256" "$TMP_DIR/$ASSET.sha256"

# ---- published SHA-256 verification BEFORE extraction -------------------------
SHA_LINES="$(awk 'END{print NR}' "$TMP_DIR/$ASSET.sha256")"
SHA_FIELDS="$(awk 'NR==1{print NF}' "$TMP_DIR/$ASSET.sha256")"
SHA_DIGEST="$(awk 'NR==1{print $1}' "$TMP_DIR/$ASSET.sha256")"
SHA_NAME="$(awk 'NR==1{print $2}' "$TMP_DIR/$ASSET.sha256")"
[ "$SHA_LINES" = 1 ] && [ "$SHA_FIELDS" = 2 ] \
  || die "published SHA-256 file is malformed: $ASSET.sha256"
printf '%s' "$SHA_DIGEST" | grep -Eq '^[0-9a-f]{64}$' \
  || die "published SHA-256 file is malformed: $ASSET.sha256"
[ "$SHA_NAME" = "$ASSET" ] \
  || die "published SHA-256 file names an unexpected asset: $SHA_NAME"
(cd "$TMP_DIR" && sha256sum -c --strict -- "$ASSET.sha256") \
  || die "published SHA-256 verification failed for $ASSET — refusing to extract or execute anything from the bundle"
step 'release archive SHA-256 verified'

# ---- safe extraction ----------------------------------------------------------
safe_extract "$TMP_DIR/$ASSET" "$TMP_DIR" "$TMP_DIR/entries.txt"
BUNDLE_DIR="$TMP_DIR/o3k-$VERSION_NO_V"
[ -d "$BUNDLE_DIR" ] || die "release archive does not contain the expected bundle directory: o3k-$VERSION_NO_V"

# ---- bundled integrity and preflight ------------------------------------------
bash "$BUNDLE_DIR/packaging/verify-release-bundle.sh" "$BUNDLE_DIR" \
  || die "release bundle verification failed for o3k-$VERSION_NO_V"
step "O3K v$VERSION_NO_V verified"
bash "$BUNDLE_DIR/packaging/preflight.sh" --profile libvirt --data-dir /var/lib/o3k \
  || die "preflight failed (--profile libvirt --data-dir /var/lib/o3k): the host does not meet the certified TestLab requirements"
step 'KVM available'

# ---- TLS identities: preserve complete set, bootstrap only when absent --------
TLS_DIR=/etc/o3k/tls
TLS_FILES="ca.pem server.pem server-key.pem agent.pem agent-key.pem agent-id agent-fingerprint"
tls_present=0
for file in $TLS_FILES; do
  if [ -e "$TLS_DIR/$file" ] || [ -L "$TLS_DIR/$file" ]; then
    [ -f "$TLS_DIR/$file" ] && [ ! -L "$TLS_DIR/$file" ] && [ -s "$TLS_DIR/$file" ] \
      || die "TLS identity is not a regular non-empty file: $TLS_DIR/$file"
    tls_present=$((tls_present + 1))
  fi
done
if [ "$tls_present" -eq 7 ]; then
  step 'TLS identities preserved'
elif [ "$tls_present" -eq 0 ]; then
  bash "$BUNDLE_DIR/packaging/bootstrap-certs.sh" \
    --output-dir "$TLS_DIR" --server-name o3k-control-plane --agent-id compute-agent \
    || die 'TLS bootstrap failed'
  step 'mTLS identities ready'
else
  die "partial TLS identity set under $TLS_DIR ($tls_present of 7 files) — refusing to regenerate valid identities; complete the set manually (never use --force)"
fi

# ---- install from the verified bundle -----------------------------------------
bash "$BUNDLE_DIR/packaging/install.sh" --profile libvirt --noninteractive \
  --binary "$BUNDLE_DIR/bin/o3kd" --compute-binary "$BUNDLE_DIR/bin/o3k-compute" \
  || die 'installation failed; the host holds recoverable O3K-owned state and re-running the installer converges'
step 'o3kd installed'
step 'o3k-compute installed'

# ---- service health (same gates as the proven clean-install harness) ----------
wait_http_ok http://127.0.0.1:18080/healthz 30 \
  || die 'o3kd did not become healthy (http://127.0.0.1:18080/healthz)'
step 'control plane ready'
wait_http_ok http://127.0.0.1:9100/readyz 30 \
  || die 'o3k-compute did not become ready (http://127.0.0.1:9100/readyz)'
step 'compute agent connected'

# ---- TestLab bootstrap (public APIs only, idempotent) -------------------------
bash "$BUNDLE_DIR/packaging/bootstrap-testlab.sh" || die 'TestLab bootstrap failed'

printf '\nO3K is ready.\n\n'
printf 'Credentials:\n'
printf '  /etc/o3k/admin-openrc\n'
printf '  /etc/o3k/clouds.yaml\n\n'
printf 'Try:\n\n'
printf '  source /etc/o3k/admin-openrc\n'
printf '  openstack server list\n'
printf '  openstack console log show test-vm\n'
