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
SYSTEM_INSTALL=0
if [[ "$PREFIX" == /usr/local && "$DATA_DIR" == /var/lib/o3k && "$CONFIG_DIR" == /etc/o3k && "$LOG_DIR" == /var/log/o3k ]]; then SYSTEM_INSTALL=1; fi
if [[ $EUID -ne 0 && ( "$PREFIX" == /usr/* || "$DATA_DIR" == /var/* || "$CONFIG_DIR" == /etc/* ) ]]; then echo "system paths require root; use sudo or explicit user paths" >&2; exit 2; fi
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ "$PROFILE" == libvirt ]]; then "$ROOT_DIR/packaging/preflight.sh" --profile libvirt; fi
if [[ -z "$BINARY" ]]; then cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --bin o3kd; BINARY="$ROOT_DIR/target/release/o3kd"; fi
[[ -x "$BINARY" ]] || { echo "binary is not executable: $BINARY" >&2; exit 1; }
if [[ "$PROFILE" == libvirt && -z "$COMPUTE_BINARY" ]]; then cargo build --release --manifest-path "$ROOT_DIR/Cargo.toml" --features libvirt --bin o3k-compute; COMPUTE_BINARY="$ROOT_DIR/target/release/o3k-compute"; fi
if [[ "$PROFILE" == libvirt ]]; then [[ -x "$COMPUTE_BINARY" ]] || { echo "compute binary is not executable: $COMPUTE_BINARY" >&2; exit 1; }; fi
if [[ $EUID -eq 0 ]]; then
  getent group o3k >/dev/null || groupadd --system o3k
  id o3k >/dev/null 2>&1 || useradd --system --gid o3k --home-dir /var/lib/o3k --shell /usr/sbin/nologin o3k
  RUN_USER=o3k
else RUN_USER="$(id -un)"; fi
install -d -m 0755 "$PREFIX/bin" "$PREFIX/share/o3k" "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"
for path in "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"; do touch "$path/.o3k-owned"; chmod 0644 "$path/.o3k-owned"; done
install -m 0755 "$BINARY" "$PREFIX/bin/o3kd"
install -m 0644 "$ROOT_DIR/packaging/o3kd.service" "$PREFIX/share/o3k/o3kd.service"
install -m 0755 "$ROOT_DIR/packaging/reset.sh" "$ROOT_DIR/packaging/uninstall.sh" "$ROOT_DIR/packaging/diagnose.sh" "$ROOT_DIR/packaging/preflight.sh" "$ROOT_DIR/packaging/bootstrap-certs.sh" "$PREFIX/share/o3k/"
if [[ "$PROFILE" == libvirt ]]; then install -m 0755 "$COMPUTE_BINARY" "$PREFIX/bin/o3k-compute"; install -m 0644 "$ROOT_DIR/packaging/o3k-compute.service" "$PREFIX/share/o3k/o3k-compute.service"; fi
ENV_FILE="$CONFIG_DIR/o3kd.env"
if [[ ! -e "$ENV_FILE" ]]; then
  umask 077
  if [[ $NONINTERACTIVE -eq 1 ]]; then PASSWORD="$(od -An -N24 -tx1 /dev/urandom | tr -d ' 
')"; else read -r -s -p "Bootstrap password (empty disables identity): " PASSWORD; echo; fi
  SIGNING_KEY="$(od -An -N48 -tx1 /dev/urandom | tr -d ' 
')"
  { printf 'O3K_DATA_DIR=%q
' "$DATA_DIR"; printf 'O3K_BOOTSTRAP_PASSWORD=%q
' "$PASSWORD"; printf 'O3K_TOKEN_SIGNING_KEY=%q
' "$SIGNING_KEY"; } >"$ENV_FILE"
  chmod 0600 "$ENV_FILE"
fi
if [[ "$PROFILE" == libvirt && ! -e "$CONFIG_DIR/o3k-compute.env" ]]; then
  umask 077
  printf 'O3K_COMPUTE_DATA_DIR=%q\nO3K_COMPUTE_PROFILE=libvirt\nO3K_COMPUTE_TLS_DIR=%q\n' "$DATA_DIR" /etc/o3k/tls >"$CONFIG_DIR/o3k-compute.env"
  chmod 0600 "$CONFIG_DIR/o3k-compute.env"
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
