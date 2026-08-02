#!/usr/bin/env bash
set -Eeuo pipefail
PREFIX=/usr/local
DATA_DIR=/var/lib/o3k
CONFIG_DIR=/etc/o3k
LOG_DIR=/var/log/o3k
PURGE=0
CONFIRM=0
while (($#)); do case "$1" in --prefix) PREFIX="$2"; shift 2;; --data-dir) DATA_DIR="$2"; shift 2;; --config-dir) CONFIG_DIR="$2"; shift 2;; --log-dir) LOG_DIR="$2"; shift 2;; --purge) PURGE=1; shift;; --yes) CONFIRM=1; shift;; *) echo "unknown option: $1" >&2; exit 2;; esac; done
[[ $PURGE -eq 0 || $CONFIRM -eq 1 ]] || { echo "--purge requires --yes" >&2; exit 2; }
validate_path() {
  local name="$1" path="$2"
  [[ -n "$path" && "$path" == /* && "$path" != / ]] || {
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
      echo "refusing symlink uninstall path for $name: $path" >&2
      exit 2
    fi
  done < <(tr '/' '\n' <<< "${path#/}")
}
validate_path prefix "$PREFIX"
for path in "$PREFIX/bin" "$PREFIX/share" "$PREFIX/share/o3k"; do
  if [[ -L "$path" ]]; then
    echo "refusing symlink uninstall path: $path" >&2
    exit 2
  fi
  if [[ -e "$path" && ! -d "$path" ]]; then
    echo "uninstall path is not a directory: $path" >&2
    exit 2
  fi
done
INSTALL_MANIFEST="$PREFIX/share/o3k/.o3k-installed"
[[ -f "$INSTALL_MANIFEST" && ! -L "$INSTALL_MANIFEST" ]] || {
  echo "refusing to remove files without an installation ownership manifest: $INSTALL_MANIFEST" >&2
  exit 2
}
MANIFEST_HEADER="o3k-installed-v1 prefix=$PREFIX"
[[ "$(head -n 1 "$INSTALL_MANIFEST")" == "$MANIFEST_HEADER" ]] || {
  echo "refusing unrecognized installation ownership manifest: $INSTALL_MANIFEST" >&2
  exit 2
}
MANIFEST_FILES=()
while IFS= read -r line; do
  [[ -n "$line" ]] || { echo "refusing malformed installation ownership manifest: $INSTALL_MANIFEST" >&2; exit 2; }
  [[ "$line" == "$MANIFEST_HEADER" ]] && continue
  case "$line" in
    bin/o3kd|bin/o3k-compute|share/o3k/o3kd.service|share/o3k/o3k-compute.service|share/o3k/reset.sh|share/o3k/uninstall.sh|share/o3k/diagnose.sh|share/o3k/preflight.sh|share/o3k/bootstrap-certs.sh|share/o3k/generate-passwords.sh)
      MANIFEST_FILES+=("$line")
      ;;
    *)
      echo "refusing unrecognized installation ownership entry: $line" >&2
      exit 2
      ;;
  esac
done < "$INSTALL_MANIFEST"
(( ${#MANIFEST_FILES[@]} > 0 )) || {
  echo "refusing empty installation ownership manifest: $INSTALL_MANIFEST" >&2
  exit 2
}
owned_marker() { [[ -f "$1/.o3k-owned" && ! -L "$1/.o3k-owned" ]] && grep -Fqx "o3k-owned-v1 path=$1" "$1/.o3k-owned"; }
SYSTEM_INSTALL=0
if [[ "$PREFIX" == /usr/local && "$DATA_DIR" == /var/lib/o3k && "$CONFIG_DIR" == /etc/o3k && "$LOG_DIR" == /var/log/o3k ]]; then SYSTEM_INSTALL=1; fi
if [[ $PURGE -eq 1 ]]; then
  for path in "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"; do
    validate_path purge-target "$path"
    if [[ -e "$path" ]] && ! owned_marker "$path"; then
      echo "refusing purge of unowned path: $path" >&2
      exit 2
    fi
  done
fi
if [[ $SYSTEM_INSTALL -eq 1 ]]; then
  command -v systemctl >/dev/null 2>&1 && systemctl disable --now o3kd.service 2>/dev/null || true
  command -v systemctl >/dev/null 2>&1 && systemctl disable --now o3k-compute.service 2>/dev/null || true
  command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
fi
if [[ $PURGE -eq 1 ]]; then
  for path in "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"; do [[ -e "$path" ]] && find "$path" -mindepth 1 -maxdepth 1 ! -name .o3k-owned -exec rm -rf -- {} +; [[ -f "$path/.o3k-owned" ]] && rm -f -- "$path/.o3k-owned"; rmdir "$path" 2>/dev/null || true; done
fi

if [[ $SYSTEM_INSTALL -eq 1 ]]; then
  rm -f -- /etc/systemd/system/o3kd.service /etc/systemd/system/o3k-compute.service
fi
for relative in "${MANIFEST_FILES[@]}"; do
  destination="$PREFIX/$relative"
  if [[ -L "$destination" ]]; then
    echo "refusing to remove symlink installation target: $destination" >&2
    exit 2
  fi
  if [[ -e "$destination" && ! -f "$destination" ]]; then
    echo "refusing to remove non-file installation target: $destination" >&2
    exit 2
  fi
  [[ -e "$destination" ]] && rm -f -- "$destination"
done
rm -f -- "$INSTALL_MANIFEST"
if [[ $PURGE -eq 1 ]]; then
  echo "o3k binaries, helper files, and owned state removed"
else
  echo "o3k binaries and helper files removed; data/config/logs preserved (use --purge --yes to remove them)"
fi
