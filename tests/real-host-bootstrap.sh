#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d)"
trap 'find "$WORK_DIR" -type f -delete 2>/dev/null || true; find "$WORK_DIR" -depth -type d -empty -delete 2>/dev/null || true' EXIT

mkdir -p "$WORK_DIR/bin" "$WORK_DIR/venv/bin" "$WORK_DIR/temp"
cat >"$WORK_DIR/bin/sudo" <<'EOF'
#!/usr/bin/env bash
set -Eeuo pipefail
if [[ "$1" == -n && "$2" == true ]]; then exit 0; fi
shift
if [[ "$1" == env ]]; then shift; while [[ "$1" == *=* ]]; do shift; done; fi
if [[ "$1" == apt-get ]]; then exit 0; fi
exec "$@"
EOF
chmod 0755 "$WORK_DIR/bin/sudo"
cat >"$WORK_DIR/venv/bin/openstack" <<'EOF'
#!/usr/bin/env bash
printf 'openstack 10.2.1\n'
EOF
chmod 0755 "$WORK_DIR/venv/bin/openstack"
cat >"$WORK_DIR/bin/genisoimage" <<'EOF'
#!/usr/bin/env bash
exit 0
EOF
chmod 0755 "$WORK_DIR/bin/genisoimage"
cat >"$WORK_DIR/bin/id" <<'EOF'
#!/usr/bin/env bash
if [[ "$1" == -un ]]; then printf 'o3k-testlab\n'; else /usr/bin/id "$@"; fi
EOF
chmod 0755 "$WORK_DIR/bin/id"

GITHUB_ENV="$WORK_DIR/github.env"
GITHUB_PATH="$WORK_DIR/github.path"
O3K_OPENSTACK_VENV="$WORK_DIR/venv"
O3K_REAL_HOST_PROTECTED_STATE="$WORK_DIR/protected-state"
OS_PASSWORD='generated-test-password'
export PATH="$WORK_DIR/bin:$PATH" GITHUB_ENV GITHUB_PATH O3K_OPENSTACK_VENV \
  O3K_REAL_HOST_PROTECTED_STATE OS_PASSWORD RUNNER_TEMP="$WORK_DIR/temp"

bash "$ROOT_DIR/scripts/real-host-bootstrap.sh" >"$WORK_DIR/stdout" 2>"$WORK_DIR/stderr"
grep -Fqx 'O3K_REAL_HOST_SERVICE_ACCOUNT=o3k-testlab' "$GITHUB_ENV"
grep -Fqx 'OS_PASSWORD=generated-test-password' "$GITHUB_ENV"
grep -Fqx "O3K_REAL_HOST_PROTECTED_PATHS=$WORK_DIR/protected-state" "$GITHUB_ENV"
grep -Fqx 'O3K_TESTLAB_PASSWORD_SOURCE=environment' "$GITHUB_ENV"
grep -Fqx "$WORK_DIR/venv/bin" "$GITHUB_PATH"
[[ "$(<"$WORK_DIR/protected-state")" == 'o3k-real-host-protected-state-v1' ]]
! grep -Fq 'generated-test-password' "$WORK_DIR/stdout" "$WORK_DIR/stderr"

printf 'keystone_admin_password: "kolla# shared\\x41-password" # comment\n' >"$WORK_DIR/passwords.yml"
GITHUB_ENV="$WORK_DIR/github-kolla.env"
GITHUB_PATH="$WORK_DIR/github-kolla.path"
O3K_KOLLA_PASSWORD_FILE="$WORK_DIR/passwords.yml"
O3K_REAL_HOST_PROTECTED_STATE="$WORK_DIR/kolla-protected-state"
unset OS_PASSWORD
export GITHUB_ENV GITHUB_PATH O3K_KOLLA_PASSWORD_FILE O3K_REAL_HOST_PROTECTED_STATE
bash "$ROOT_DIR/scripts/real-host-bootstrap.sh" >"$WORK_DIR/kolla-stdout" 2>"$WORK_DIR/kolla-stderr"
grep -Fqx 'OS_PASSWORD=kolla# sharedA-password' "$GITHUB_ENV"
grep -Fqx 'O3K_TESTLAB_PASSWORD_SOURCE=kolla-passwords-yml' "$GITHUB_ENV"
grep -Fqx '::add-mask::kolla# sharedA-password' "$WORK_DIR/kolla-stdout"
! grep -Fq 'kolla# sharedA-password' "$WORK_DIR/kolla-stderr"

echo "real-host bootstrap tests passed"
