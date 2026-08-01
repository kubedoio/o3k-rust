#!/usr/bin/env bash
set -Eeuo pipefail

# Prepare only disposable workflow dependencies. This script never creates
# OpenStack resources and never prints credentials. Existing credentials are
# preserved, matching the idempotent kolla-genpwd model.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER_TEMP="${RUNNER_TEMP:-${TMPDIR:-/tmp}}"
GITHUB_ENV="${GITHUB_ENV:-/dev/null}"
GITHUB_PATH="${GITHUB_PATH:-/dev/null}"
OPENSTACK_VENV="${O3K_OPENSTACK_VENV:-${RUNNER_TEMP}/o3k-openstack-venv}"
OPENSTACK_VERSION="${O3K_OPENSTACK_VERSION:-10.2.1}"
PASSWORD_FILE="${O3K_PASSWORD_FILE:-/etc/o3k/o3kd.env}"
PROTECTED_PATH="${O3K_REAL_HOST_PROTECTED_STATE:-${RUNNER_TEMP}/o3k-protected-state}"

fail() {
  echo "real-host bootstrap failed: $1" >&2
  exit 1
}

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v sudo >/dev/null 2>&1 || fail "sudo is unavailable"
sudo -n true 2>/dev/null || fail "passwordless sudo is required for disposable host dependencies"

if ! command -v genisoimage >/dev/null 2>&1; then
  sudo env DEBIAN_FRONTEND=noninteractive apt-get update -qq
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends genisoimage python3-venv
fi

if [[ ! -x "$OPENSTACK_VENV/bin/openstack" ]]; then
  python3 -m venv "$OPENSTACK_VENV"
  "$OPENSTACK_VENV/bin/python" -m pip install --disable-pip-version-check --no-input \
    "python-openstackclient==${OPENSTACK_VERSION}"
fi
"$OPENSTACK_VENV/bin/openstack" --version >/dev/null 2>&1 || fail "OpenStack CLI installation is unusable"
printf '%s/bin\n' "$OPENSTACK_VENV" >>"$GITHUB_PATH"
export PATH="$OPENSTACK_VENV/bin:$PATH"

runner_account="$(id -un)"
[[ "$runner_account" != root ]] || fail "workflow must not run as root"
printf 'O3K_REAL_HOST_SERVICE_ACCOUNT=%s\n' "$runner_account" >>"$GITHUB_ENV"

umask 077
install -m 0600 /dev/null "$PROTECTED_PATH"
printf 'o3k-real-host-protected-state-v1\n' >"$PROTECTED_PATH"
printf 'O3K_REAL_HOST_PROTECTED_PATHS=%s\n' "$PROTECTED_PATH" >>"$GITHUB_ENV"

password="${OS_PASSWORD:-}"
if [[ -z "$password" ]]; then
  if [[ -r "$PASSWORD_FILE" ]]; then
    password="$(awk -F= '$1 == "O3K_BOOTSTRAP_PASSWORD" {sub(/^[^=]*=/, ""); print; exit}' "$PASSWORD_FILE")"
  elif sudo -n test -r "$PASSWORD_FILE" 2>/dev/null; then
    password="$(sudo -n awk -F= '$1 == "O3K_BOOTSTRAP_PASSWORD" {sub(/^[^=]*=/, ""); print; exit}' "$PASSWORD_FILE")"
  fi
fi
[[ -n "$password" ]] || fail "no existing O3K bootstrap password or protected OS_PASSWORD secret"
[[ "$password" != *$'\n'* && "$password" != *$'\r'* ]] || fail "bootstrap password contains a newline"
printf 'OS_PASSWORD=%s\n' "$password" >>"$GITHUB_ENV"

echo "real-host bootstrap prepared disposable CLI/config-drive dependencies"
