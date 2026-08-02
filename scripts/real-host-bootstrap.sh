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
KOLLA_PASSWORD_FILE="${O3K_KOLLA_PASSWORD_FILE:-/etc/kolla/passwords.yml}"
KOLLA_OPENRC_FILE="${O3K_KOLLA_OPENRC_FILE:-/etc/kolla/admin-openrc.sh}"
PROTECTED_PATH="${O3K_REAL_HOST_PROTECTED_STATE:-${RUNNER_TEMP}/o3k-protected-state}"

fail() {
  echo "real-host bootstrap failed: $1" >&2
  exit 1
}

command -v python3 >/dev/null 2>&1 || fail "python3 is unavailable"
command -v sudo >/dev/null 2>&1 || fail "sudo is unavailable"
sudo -n true 2>/dev/null || fail "passwordless sudo is required for disposable host dependencies"

if ! command -v xorriso >/dev/null 2>&1; then
  sudo env DEBIAN_FRONTEND=noninteractive apt-get update -qq
  sudo env DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends xorriso python3-venv
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
password_source="environment"
read_password_file() {
  local path="$1" expression="$2" value=""
  if [[ -r "$path" ]]; then
    if [[ "$expression" == '-F= '* ]]; then
      value="$(awk -F= "${expression#-F= }" "$path")"
    else
      value="$(awk "$expression" "$path")"
    fi
  elif sudo -n test -r "$path" 2>/dev/null; then
    if [[ "$expression" == '-F= '* ]]; then
      value="$(sudo -n awk -F= "${expression#-F= }" "$path")"
    else
      value="$(sudo -n awk "$expression" "$path")"
    fi
  fi
  printf '%s' "$value"
}

normalize_password_scalar() {
  printf '%s' "$1" | python3 -c '
import json
import sys
import ast

value = sys.stdin.read().rstrip("\r\n")
quote = None
escaped = False
for index, character in enumerate(value):
    if quote == chr(34) and escaped:
        escaped = False
    elif quote == chr(34) and character == chr(92):
        escaped = True
    elif character in (chr(34), chr(39)):
        quote = None if quote == character else character if quote is None else quote
    elif quote is None and character == chr(35) and index > 0 and value[index - 1].isspace():
        value = value[:index].rstrip()
        break
if len(value) >= 2 and value[0] == value[-1] == chr(34):
    try:
        value = json.loads(value)
    except json.JSONDecodeError:
        value = ast.literal_eval(value)
elif len(value) >= 2 and value[0] == value[-1] == chr(39):
    value = value[1:-1].replace(chr(39) * 2, chr(39))
print(value, end="")
'
}

if [[ -z "$password" ]]; then
  password="$(read_password_file "$PASSWORD_FILE" \
    '-F= $1 == "O3K_BOOTSTRAP_PASSWORD" {sub(/^[^=]*=/, ""); print; exit}')"
  password_source="o3k-env-file"
fi
if [[ -z "$password" ]]; then
  # Kolla-Ansible's generated passwords.yml is the canonical shared source
  # for an existing deployment. Read only the admin credential; do not run or
  # source an openrc file, since it may contain unrelated shell code.
  password="$(read_password_file "$KOLLA_PASSWORD_FILE" \
    '$1 == "keystone_admin_password:" {sub(/^[^:]*:[[:space:]]*/, ""); print; exit}')"
  password="$(normalize_password_scalar "$password")"
  password_source="kolla-passwords-yml"
fi
if [[ -z "$password" ]]; then
  password="$(read_password_file "$KOLLA_OPENRC_FILE" \
    '$1 == "export" && $2 ~ /^OS_PASSWORD=/ {sub(/^export OS_PASSWORD=/, ""); print; exit}')"
  password="$(normalize_password_scalar "$password")"
  password_source="kolla-admin-openrc"
fi
if [[ -z "$password" ]]; then
  generated_password_file="$(mktemp "$RUNNER_TEMP/.o3k-passwords.XXXXXX")"
  trap 'rm -f -- "$generated_password_file"' EXIT
  bash "$ROOT_DIR/scripts/generate-passwords.sh" \
    --output "$generated_password_file" --kolla-password-file "$KOLLA_PASSWORD_FILE" \
    >/dev/null || fail "credential generation failed"
  password="$(read_password_file "$generated_password_file" \
    '-F= $1 == "O3K_BOOTSTRAP_PASSWORD" {sub(/^[^=]*=/, ""); print; exit}')"
  password="$(normalize_password_scalar "$password")"
  password_source="generated-passwords"
fi
[[ -n "$password" ]] || fail "no existing O3K bootstrap password or protected OS_PASSWORD secret"
[[ "$password" != *$'\n'* && "$password" != *$'\r'* ]] || fail "bootstrap password contains a newline"
printf '::add-mask::%s\n' "$password"
printf 'OS_PASSWORD=%s\n' "$password" >>"$GITHUB_ENV"
printf 'O3K_TESTLAB_PASSWORD_SOURCE=%s\n' "$password_source" >>"$GITHUB_ENV"

echo "real-host bootstrap prepared disposable CLI/config-drive dependencies"
