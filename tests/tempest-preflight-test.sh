#!/usr/bin/env bash
set -Eeuo pipefail

# Deterministic regression test for tests/tempest-preflight.sh validation
# logic. Runs the preflight in self-test mode (no network, no install, no real
# Tempest) with injected version/discovery values and asserts the pass/fail
# decisions. Guards against the preflight ever accepting a wrong version, a
# missing allowlisted test, or a zero-test discovery as useful evidence.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
PREFLIGHT="${ROOT_DIR}/tests/tempest-preflight.sh"

SELFTEST_ENV=(O3K_PREFLIGHT_SELFTEST=1 O3K_PREFLIGHT_VENV_PY="$(command -v python3)")

# Discovered test IDs use the stestr "[id-<uuid>[,tags]]" suffix form.
ALLOWLIST=(
  "tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token"
  "tempest.api.volume.test_volumes_get.VolumesGetTest.test_volume_create_get_update_delete"
  "tempest.api.volume.test_volumes_list.VolumesListTestJSON.test_volume_list_with_details"
)

WORK="$(mktemp -d "${TMPDIR:-/tmp}/o3k-tempest-preflight-test.XXXXXX")"
trap 'rm -rf -- "${WORK}"' EXIT

printf '%s\n' \
  "${ALLOWLIST[0]}[id-6f8e4436-fc96-4282-8122-e41df57197a9]" \
  "${ALLOWLIST[1]}[id-27fb0e9f-fb64-41dd-8bdb-1ffa762f0d51,smoke]" \
  "${ALLOWLIST[2]}[id-adcbb5a7-5ad8-4b61-bd10-5380e111a877]" \
  > "${WORK}/discovered-good.txt"
printf '%s\n' \
  "${ALLOWLIST[0]}[id-6f8e4436-fc96-4282-8122-e41df57197a9]" \
  "${ALLOWLIST[1]}[id-27fb0e9f-fb64-41dd-8bdb-1ffa762f0d51,smoke]" \
  > "${WORK}/discovered-missing.txt"
: > "${WORK}/discovered-empty.txt"

expect() {
  # expect <expected-exit> <label> -- <env assignments...>
  local expected="$1" label="$2"; shift 2
  shift  # --
  local workdir="${WORK}/run-$(printf '%s' "$label" | tr -cd '[:alnum:]')"
  rm -rf "${workdir}"
  local ret=0
  env "${SELFTEST_ENV[@]}" \
    O3K_PREFLIGHT_TEMPEST_VERSION="46.0.0" \
    O3K_PREFLIGHT_PLUGIN_VERSION="1.21.0" \
    O3K_PREFLIGHT_DISCOVERED_TESTS="${WORK}/discovered-good.txt" \
    O3K_PREFLIGHT_WORKDIR="${workdir}" \
    "$@" \
    bash "${PREFLIGHT}" >/dev/null 2>&1 || ret=$?
  if [ "${ret}" -ne "${expected}" ]; then
    echo "FAIL: ${label}: expected exit ${expected}, got ${ret}" >&2
    exit 1
  fi
  echo "ok: ${label}"
}

# Good path: exact versions + full discovery -> pass.
expect 0 "good-path" --
# Wrong tempest version must fail.
expect 1 "wrong-tempest-version" -- O3K_PREFLIGHT_TEMPEST_VERSION="45.0.0"
# Wrong plugin version must fail.
expect 1 "wrong-plugin-version" -- O3K_PREFLIGHT_PLUGIN_VERSION="1.19.0"
# An allowlisted test missing from discovery must fail.
expect 1 "missing-allowlisted-test" -- O3K_PREFLIGHT_DISCOVERED_TESTS="${WORK}/discovered-missing.txt"
# Zero-test discovery must fail.
expect 1 "empty-discovery" -- O3K_PREFLIGHT_DISCOVERED_TESTS="${WORK}/discovered-empty.txt"
# A missing dedicated venv (no valid python) must fail in real mode semantics.
expect 1 "invalid-venv-python" -- O3K_PREFLIGHT_VENV_PY="${WORK}/no-such-python" O3K_PREFLIGHT_TEMPEST_VERSION="46.0.0" O3K_PREFLIGHT_PLUGIN_VERSION="1.21.0"

echo "tempest preflight regression tests passed"
