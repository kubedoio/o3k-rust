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

ln -s "$output" "$WORK_DIR/output-link"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/output-link" >/dev/null 2>&1; then
  echo "generator accepted a symlink output" >&2
  exit 1
fi

mkdir "$WORK_DIR/parent-target"
ln -s "$WORK_DIR/parent-target" "$WORK_DIR/parent-link"
if bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$WORK_DIR/parent-link/o3kd.env" >/dev/null 2>&1; then
  echo "generator accepted a symlinked parent" >&2
  exit 1
fi

# Reset+reinstall contract: regenerating over an existing file must be
# byte-idempotent (same values reused, same line positions) so the
# installer's configuration ledger keeps matching across a reinstall. The
# fail-before defect reordered the generated keys and made every reinstall
# fail the operator-modified configuration guard.
before="$(sha256sum "$output" | awk '{print $1}')"
if ! bash "$ROOT_DIR/scripts/generate-passwords.sh" --output "$output" >/dev/null 2>&1; then
  echo "regeneration over an existing file failed" >&2
  exit 1
fi
after="$(sha256sum "$output" | awk '{print $1}')"
[[ "$before" == "$after" ]] || { echo "regeneration changed the credential file" >&2; exit 1; }

echo "password generator tests passed"
