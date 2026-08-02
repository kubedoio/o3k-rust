#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-passwords.XXXXXX")"
trap 'rm -rf -- "$WORK_DIR"' EXIT

output="$WORK_DIR/o3kd.env"
kolla="$WORK_DIR/passwords.yml"
printf 'keystone_admin_password: "kolla# shared-password" # ignored comment\n' >"$kolla"

stdout="$WORK_DIR/stdout"
bash "$ROOT_DIR/scripts/generate-passwords.sh" \
  --output "$output" --kolla-password-file "$kolla" >"$stdout"
[[ "$(awk -F= '$1 == "O3K_BOOTSTRAP_PASSWORD" {print substr($0, index($0, "=") + 1)}' "$output")" == 'kolla#\ shared-password' ]]
key="$(awk -F= '$1 == "O3K_TOKEN_SIGNING_KEY" {print substr($0, index($0, "=") + 1)}' "$output")"
[[ "$key" =~ ^[[:xdigit:]]{96}$ ]]
[[ "$(stat -c '%a' "$output")" == 600 ]]
! grep -Fq 'kolla# shared-password' "$stdout"

cp "$output" "$WORK_DIR/first"
bash "$ROOT_DIR/scripts/generate-passwords.sh" \
  --output "$output" --kolla-password-file "$WORK_DIR/missing.yml" >/dev/null
cmp -s "$WORK_DIR/first" "$output"

printf 'O3K_DATA_DIR=%q\n' "$WORK_DIR/data" >>"$output"
grep -Fqx "O3K_DATA_DIR=$WORK_DIR/data" "$output"

printf 'O3K_TOKEN_SIGNING_KEY=short\n' >"$WORK_DIR/weak.env"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/weak.env" >/dev/null 2>&1; then
  echo "generator accepted a weak existing signing key" >&2
  exit 1
fi

printf 'O3K_TOKEN_SIGNING_KEY=%s\n' "$(printf 'a%.0s' {1..65})" >"$WORK_DIR/odd.env"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/odd.env" >/dev/null 2>&1; then
  echo "generator accepted an odd-length signing key" >&2
  exit 1
fi

ln -s "$output" "$WORK_DIR/output-link"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/output-link" >/dev/null 2>&1; then
  echo "generator accepted a symlink output" >&2
  exit 1
fi

printf 'sentinel\n' >"$WORK_DIR/lock-target"
ln -s "$WORK_DIR/lock-target" "$output.lock"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$output" >/dev/null 2>&1; then
  echo "generator followed a lock symlink" >&2
  exit 1
fi
grep -Fqx 'sentinel' "$WORK_DIR/lock-target"

mkdir "$WORK_DIR/parent-target"
ln -s "$WORK_DIR/parent-target" "$WORK_DIR/parent-link"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/parent-link/o3kd.env" >/dev/null 2>&1; then
  echo "generator accepted a symlinked parent" >&2
  exit 1
fi

echo "password generator tests passed"
