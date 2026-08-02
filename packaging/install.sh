#!/usr/bin/env bash
set -Eeuo pipefail
PREFIX=/usr/local
DATA_DIR=/var/lib/o3k
CONFIG_DIR=/etc/o3k
LOG_DIR=/var/log/o3k
BINARY=
COMPUTE_BINARY=
PROFILE=fake
NONINTERACTIVE=0
while (($#)); do
  case "$1" in
    --prefix) PREFIX="$2"; shift 2;;
    --data-dir) DATA_DIR="$2"; shift 2;;
    --config-dir) CONFIG_DIR="$2"; shift 2;;
    --log-dir) LOG_DIR="$2"; shift 2;;
    --binary) BINARY="$2"; shift 2;;
    --compute-binary) COMPUTE_BINARY="$2"; shift 2;;
    --profile) PROFILE="$2"; shift 2;;
    --noninteractive) NONINTERACTIVE=1; shift;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done
[[ "$PROFILE" == fake || "$PROFILE" == libvirt ]] || { echo "profile must be fake or libvirt" >&2; exit 2; }
[[ -n "$PREFIX" && -n "$DATA_DIR" && -n "$CONFIG_DIR" && -n "$LOG_DIR" ]] || { echo "installation paths must not be empty" >&2; exit 2; }
validate_install_path() {
  local name="$1" path="$2"
  [[ "$path" == /* && "$path" != / ]] || {
    echo "$name must be an absolute non-root path: $path" >&2
    exit 2
  }
  local current=/ component
  while IFS= read -r component; do
    [[ -n "$component" ]] || continue
    case "$component" in
      .|..)
        echo "$name must not contain lexical dot components: $path" >&2
        exit 2
        ;;
    esac
    current="$current/$component"
    if [[ -L "$current" ]]; then
      echo "refusing symlink installation path for $name: $path" >&2
      exit 2
    fi
  done < <(tr '/' '\n' <<< "${path#/}")
  if [[ -e "$path" && ! -d "$path" ]]; then
    echo "installation path for $name is not a directory: $path" >&2
    exit 2
  fi
}
validate_install_path prefix "$PREFIX"
validate_install_path data-dir "$DATA_DIR"
validate_install_path config-dir "$CONFIG_DIR"
validate_install_path log-dir "$LOG_DIR"
SYSTEM_INSTALL=0
if [[ "$PREFIX" == /usr/local && "$DATA_DIR" == /var/lib/o3k && "$CONFIG_DIR" == /etc/o3k && "$LOG_DIR" == /var/log/o3k ]]; then SYSTEM_INSTALL=1; fi
if [[ $EUID -ne 0 && ( "$PREFIX" == /usr/* || "$DATA_DIR" == /var/* || "$CONFIG_DIR" == /etc/* ) ]]; then echo "system paths require root; use sudo or explicit user paths" >&2; exit 2; fi
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$PROFILE" == libvirt ]]; then "$ROOT_DIR/packaging/preflight.sh" --profile libvirt --data-dir "$DATA_DIR"; fi
if [[ -z "$BINARY" ]]; then
  if [[ -x "$ROOT_DIR/bin/o3kd" ]]; then
    BINARY="$ROOT_DIR/bin/o3kd"
  else
    cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd
    BINARY="$ROOT_DIR/target/release/o3kd"
  fi
fi
[[ -x "$BINARY" ]] || { echo "binary is not executable: $BINARY" >&2; exit 1; }
if [[ "$PROFILE" == libvirt && -z "$COMPUTE_BINARY" ]]; then
  if [[ -x "$ROOT_DIR/bin/o3k-compute" ]]; then
    COMPUTE_BINARY="$ROOT_DIR/bin/o3k-compute"
  else
    cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --features libvirt --bin o3k-compute
    COMPUTE_BINARY="$ROOT_DIR/target/release/o3k-compute"
  fi
fi
if [[ "$PROFILE" == libvirt ]]; then [[ -x "$COMPUTE_BINARY" ]] || { echo "compute binary is not executable: $COMPUTE_BINARY" >&2; exit 1; }; fi
TLS_DIR="$CONFIG_DIR/tls"
if [[ "$PROFILE" == libvirt ]]; then
  [[ -d "$TLS_DIR" && ! -L "$TLS_DIR" ]] || { echo "libvirt TLS directory is missing or unsafe: $TLS_DIR" >&2; exit 2; }
  for file in ca.pem server.pem server-key.pem agent.pem agent-key.pem agent-id agent-fingerprint; do
    [[ -f "$TLS_DIR/$file" && ! -L "$TLS_DIR/$file" && -s "$TLS_DIR/$file" ]] || {
      echo "libvirt TLS bootstrap is incomplete: $TLS_DIR/$file" >&2
      exit 2
    }
  done
  FINGERPRINT="$(<"$TLS_DIR/agent-fingerprint")"
  [[ "$FINGERPRINT" =~ ^[0-9a-fA-F]{64}$ ]] || { echo "agent fingerprint is invalid" >&2; exit 2; }
fi
if [[ $EUID -eq 0 ]]; then
  getent group o3k >/dev/null || groupadd --system o3k
  id o3k >/dev/null 2>&1 || useradd --system --gid o3k --home-dir /var/lib/o3k --shell /usr/sbin/nologin o3k
  RUN_USER=o3k
else RUN_USER="$(id -un)"; fi

mark_owned_dir() {
  local path="$1" marker="$1/.o3k-owned"
  if [[ -e "$path" ]]; then
    [[ -d "$path" && ! -L "$path" ]] || { echo "refusing non-directory install path: $path" >&2; exit 2; }
    if [[ -e "$marker" ]]; then
      [[ -f "$marker" && ! -L "$marker" ]] || { echo "refusing invalid ownership marker: $marker" >&2; exit 2; }
      grep -Fqx "o3k-owned-v1 path=$path" "$marker" || { echo "refusing unrecognized ownership marker: $marker" >&2; exit 2; }
    elif [[ -n "$(find "$path" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
      echo "refusing to claim populated unowned directory: $path" >&2
      exit 2
    else
      printf 'o3k-owned-v1 path=%s\n' "$path" >"$marker"
    fi
  else
    install -d -m 0755 "$path"
    printf 'o3k-owned-v1 path=%s\n' "$path" >"$marker"
  fi
  chmod 0644 "$marker"
}

for path in "$PREFIX/bin" "$PREFIX/share" "$PREFIX/share/o3k"; do
  if [[ -L "$path" ]]; then
    echo "refusing symlink installation path: $path" >&2
    exit 2
  fi
  if [[ -e "$path" && ! -d "$path" ]]; then
    echo "installation child path is not a directory: $path" >&2
    exit 2
  fi
done
install -d -m 0755 "$PREFIX/bin" "$PREFIX/share/o3k"
INSTALL_MANIFEST="$PREFIX/share/o3k/.o3k-installed"
if [[ -L "$INSTALL_MANIFEST" || ( -e "$INSTALL_MANIFEST" && ! -f "$INSTALL_MANIFEST" ) ]]; then
  echo "refusing invalid installation ownership manifest: $INSTALL_MANIFEST" >&2
  exit 2
fi
manifest_owns() {
  [[ -f "$INSTALL_MANIFEST" ]] && grep -Fqx "$1" "$INSTALL_MANIFEST"
}
if [[ -f "$INSTALL_MANIFEST" ]] && ! grep -Fqx "o3k-installed-v1 prefix=$PREFIX" "$INSTALL_MANIFEST"; then
  echo "refusing unrecognized installation ownership manifest: $INSTALL_MANIFEST" >&2
  exit 2
fi
install_owned_file() {
  local source="$1" destination="$2" relative="$3" mode="$4"
  if [[ -L "$destination" ]]; then
    echo "refusing symlink installation target: $destination" >&2
    exit 2
  fi
  if [[ -e "$destination" ]] && ! manifest_owns "$relative"; then
    echo "refusing to overwrite foreign installation file: $destination" >&2
    exit 2
  fi
  install -m "$mode" "$source" "$destination"
}
for path in "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"; do mark_owned_dir "$path"; done
INSTALLED_FILES=(
  bin/o3kd
  share/o3k/o3kd.service
  share/o3k/reset.sh
  share/o3k/uninstall.sh
  share/o3k/diagnose.sh
  share/o3k/preflight.sh
  share/o3k/bootstrap-certs.sh
  share/o3k/generate-passwords.sh
)
install_owned_file "$BINARY" "$PREFIX/bin/o3kd" bin/o3kd 0755
install_owned_file "$ROOT_DIR/packaging/o3kd.service" "$PREFIX/share/o3k/o3kd.service" share/o3k/o3kd.service 0644
for file in reset.sh uninstall.sh diagnose.sh preflight.sh bootstrap-certs.sh; do
  install_owned_file "$ROOT_DIR/packaging/$file" "$PREFIX/share/o3k/$file" "share/o3k/$file" 0755
done
install_owned_file "$ROOT_DIR/scripts/generate-passwords.sh" \
  "$PREFIX/share/o3k/generate-passwords.sh" share/o3k/generate-passwords.sh 0755
if [[ "$PROFILE" == libvirt ]]; then
  install_owned_file "$COMPUTE_BINARY" "$PREFIX/bin/o3k-compute" bin/o3k-compute 0755
  install_owned_file "$ROOT_DIR/packaging/o3k-compute.service" "$PREFIX/share/o3k/o3k-compute.service" share/o3k/o3k-compute.service 0644
  INSTALLED_FILES+=(bin/o3k-compute share/o3k/o3k-compute.service)
fi
MANIFEST_TEMP="$INSTALL_MANIFEST.tmp-$$"
{
  printf 'o3k-installed-v1 prefix=%s\n' "$PREFIX"
  printf '%s\n' "${INSTALLED_FILES[@]}"
} >"$MANIFEST_TEMP"
mv -f -- "$MANIFEST_TEMP" "$INSTALL_MANIFEST"
chmod 0644 "$INSTALL_MANIFEST"
ENV_FILE="$CONFIG_DIR/o3kd.env"
O3K_PASSWORD_FILE="$ENV_FILE" \
  O3K_KOLLA_PASSWORD_FILE="${O3K_KOLLA_PASSWORD_FILE:-/etc/kolla/passwords.yml}" \
  bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$ENV_FILE"
if ! grep -q '^O3K_DATA_DIR=' "$ENV_FILE"; then
  umask 077
  printf 'O3K_DATA_DIR=%q\n' "$DATA_DIR" >>"$ENV_FILE"
  chmod 0600 "$ENV_FILE"
fi
if [[ "$PROFILE" == libvirt ]]; then
  for file in ca.pem server.pem server-key.pem agent.pem agent-key.pem agent-id agent-fingerprint; do
    [[ -s "$TLS_DIR/$file" ]] || { echo "libvirt TLS bootstrap is incomplete: $TLS_DIR/$file" >&2; exit 2; }
  done
  if [[ $EUID -eq 0 ]]; then
    chgrp o3k "$TLS_DIR" "$TLS_DIR"/*
    chmod 0750 "$TLS_DIR"
    chmod 0640 "$TLS_DIR"/*
  fi
  if [[ ! -e "$CONFIG_DIR/o3k-compute.env" ]]; then
    umask 077
    printf 'O3K_COMPUTE_DATA_DIR=%q\nO3K_COMPUTE_PROFILE=libvirt\nO3K_COMPUTE_TLS_DIR=%q\n' "$DATA_DIR" "$TLS_DIR" >"$CONFIG_DIR/o3k-compute.env"
    chmod 0600 "$CONFIG_DIR/o3k-compute.env"
  fi
  for setting in \
    "O3K_PROVIDER=libvirt" \
    "O3K_COMPUTE_SERVER_CERTIFICATE=$TLS_DIR/server.pem" \
    "O3K_COMPUTE_SERVER_PRIVATE_KEY=$TLS_DIR/server-key.pem" \
    "O3K_COMPUTE_CLIENT_CA=$TLS_DIR/ca.pem" \
    "O3K_COMPUTE_AUTHORIZED_AGENTS=$(<"$TLS_DIR/agent-id")=$FINGERPRINT"; do
    key="${setting%%=*}"
    grep -q "^${key}=" "$ENV_FILE" || printf '%s\n' "$setting" >>"$ENV_FILE"
  done
  install -m 0640 "$TLS_DIR/agent-id" "$DATA_DIR/agent-id"
fi
if [[ $EUID -eq 0 && $SYSTEM_INSTALL -eq 1 ]]; then
  chown -R o3k:o3k "$DATA_DIR" "$LOG_DIR"
  install -m 0644 "$ROOT_DIR/packaging/o3kd.service" /etc/systemd/system/o3kd.service
  if [[ "$PROFILE" == libvirt ]]; then install -m 0644 "$ROOT_DIR/packaging/o3k-compute.service" /etc/systemd/system/o3k-compute.service; fi
  systemctl daemon-reload
  systemctl enable --now o3kd.service
  if [[ "$PROFILE" == libvirt ]]; then systemctl enable --now o3k-compute.service; fi
fi
echo "installed o3kd profile=$PROFILE at $PREFIX/bin/o3kd; data=$DATA_DIR; config=$ENV_FILE; user=$RUN_USER"
