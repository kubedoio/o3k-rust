#!/usr/bin/env bash
set -Eeuo pipefail

# Deterministic regression test for the Tempest/Cinder virtualenv isolation and
# for the Gate B (lifecycle) / Gate C (Tempest) verdict independence in the
# protected real-Cinder runner. All assertions are static against the runner
# source: no protected-runner dispatch is required to prove basic Tempest
# environment correctness.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RUNNER="${ROOT_DIR}/scripts/real-cinder-testbed-runner.sh"
SUBSET="${ROOT_DIR}/tests/tempest-cinder-subset.sh"

[ -f "${RUNNER}" ] || { echo "runner not found" >&2; exit 1; }

# 1. Tempest must never be installed into the Cinder venv.
if grep -nE '"\$\{VENV_DIR\}/bin/pip" install[^\n]*tempest' "${RUNNER}"; then
  echo "Tempest is installed into the Cinder venv" >&2
  exit 1
fi
if grep -nE '\$\{VENV_DIR\}/bin/tempest' "${RUNNER}"; then
  echo "runner invokes tempest from the Cinder venv" >&2
  exit 1
fi
if grep -nE 'PATH="\$\{VENV_DIR\}/bin:\$\{PATH\}"' "${RUNNER}" | grep -iE 'tempest|subset'; then
  echo "runner prepends the Cinder venv to PATH for Tempest" >&2
  exit 1
fi

# 2. The dedicated Tempest venv is the only environment used for Tempest.
grep -q 'export O3K_TEMPEST_VENV="${STATE_ROOT}/tempest-venv"' "${RUNNER}"
grep -q '"${O3K_TEMPEST_VENV}/bin/pip" install -q "tempest==${TEMPEST_PIN}"' "${RUNNER}"
grep -q 'PATH="${O3K_TEMPEST_VENV}/bin:${PATH}"' "${RUNNER}"
grep -q 'O3K_TEMPEST_VENV}/bin/python' "${RUNNER}"
grep -q 'O3K_PREFLIGHT_SKIP_INSTALL=1' "${RUNNER}"
grep -q 'tests/tempest-preflight.sh' "${RUNNER}"

# 3. The subset script uses explicit dedicated-venv binaries and fails
#    explicitly when the environment is invalid.
grep -q 'TEMPEST_VENV_PY="${O3K_TEMPEST_VENV:-}/bin/python"' "${SUBSET}"
grep -q '"${TEMPEST_VENV_PY}" -c "import tempest"' "${SUBSET}"
grep -q 'dedicated Tempest venv is invalid' "${SUBSET}"
if grep -nE 'command -v tempest' "${SUBSET}"; then
  echo "subset resolves tempest from ambient PATH" >&2
  exit 1
fi

# 4. Gate C failures cannot invalidate the Gate B lifecycle verdict: the
#    lifecycle status is bound before the Tempest phase and the Tempest phase
#    is guarded so it never flips it.
grep -q 'LIFECYCLE_PASSED="failed"' "${RUNNER}"
grep -q 'LIFECYCLE_PASSED="passed"' "${RUNNER}"
LIFECYCLE_BIND_LINE="$(grep -n 'LIFECYCLE_PASSED="passed"' "${RUNNER}" | head -n1 | cut -d: -f1)"
TEMPEST_PHASE_LINE="$(grep -n 'RUN_PHASE="tempest"' "${RUNNER}" | cut -d: -f1)"
[ -n "${LIFECYCLE_BIND_LINE}" ] && [ -n "${TEMPEST_PHASE_LINE}" ] \
  && [ "${LIFECYCLE_BIND_LINE}" -lt "${TEMPEST_PHASE_LINE}" ]
grep -q 'if PATH="${O3K_TEMPEST_VENV}/bin:${PATH}" bash' "${RUNNER}"
grep -q 'TEMPEST_EXECUTION_STATUS="harness-error"' "${RUNNER}"

# 5. The evidence manifest and runner result keep the verdicts separate.
grep -q 'lifecycle_status: ${LIFECYCLE_PASSED}' "${RUNNER}"
grep -q 'tempest_preflight: ${TEMPEST_PREFLIGHT_STATUS}' "${RUNNER}"
grep -q 'tempest_execution: ${TEMPEST_EXECUTION_STATUS}' "${RUNNER}"
grep -q '"lifecycle_status": lifecycle_status' "${RUNNER}"
grep -q '"tempest_preflight": tempest_preflight' "${RUNNER}"
grep -q '"tempest_execution": tempest_execution' "${RUNNER}"

# 6. A zero-test Tempest summary is never recorded as useful evidence.
grep -q 'no Tempest test was executed' "${RUNNER}"
grep -q 'zero Tempest tests were executed' "${ROOT_DIR}/tests/tempest-summary.py"
grep -q 'passed|failed|harness-error' "${RUNNER}"

# 7. Guest-level observation is honest (never hardcoded passed).
grep -q 'GUEST_OBSERVATION_STATE="not-proven"' "${RUNNER}"
grep -q 'guest_device_observation: ${GUEST_OBSERVATION_STATE}' "${RUNNER}"
grep -q '"status": guest_observation if guest_observation in ("passed", "not-proven")' "${RUNNER}"

# 8. The removed grep-based assertions were replaced with structured parsing.
grep -q 'xml.etree.ElementTree' "${RUNNER}"
grep -q 'no o3k disk ownership serial in domain XML' "${RUNNER}"
grep -q 'json.load(sys.stdin)' "${RUNNER}"

# 9. The runner never executes a tempest binary from an ambient path lookup
#    inside the tempest phase (explicit venv paths only).
if grep -n 'RUN_PHASE="tempest"' -A400 "${RUNNER}" | grep -nE '^\s*"?tempest"?\s' | grep -v 'tempest-' | grep -vE 'TEMPEST_|tempest-(venv|workspace|install|preflight|evid|status|run|cinder|summary|log|results|test|clouds)' >/dev/null; then
  echo "possible ambient tempest invocation in the tempest phase" >&2
  exit 1
fi

echo "tempest runner isolation tests passed"
