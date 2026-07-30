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
command -v systemctl >/dev/null 2>&1 && systemctl disable --now o3kd.service 2>/dev/null || true
rm -f "$PREFIX/bin/o3kd" "$PREFIX/share/o3k/o3kd.service" /etc/systemd/system/o3kd.service
command -v systemctl >/dev/null 2>&1 && systemctl daemon-reload 2>/dev/null || true
if [[ $PURGE -eq 1 ]]; then
  for path in "$DATA_DIR" "$CONFIG_DIR" "$LOG_DIR"; do [[ -n "$path" && "$path" == /* && "$path" != / ]] || { echo "refusing unsafe purge path" >&2; exit 2; }; rm -rf -- "$path"; done
  echo "o3k binaries and owned state removed"
else echo "o3k binaries removed; data/config/logs preserved (use --purge --yes to remove them)"; fi

