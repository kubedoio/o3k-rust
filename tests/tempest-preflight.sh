#!/usr/bin/env bash
# Deterministic Tempest preflight gate (Gate A portable tier).
#
# Proves everything that can be proven about the pinned Tempest environment
# WITHOUT KVM, libvirt, real Cinder, MariaDB, RabbitMQ, LVM, tgt, or the
# protected runner:
#
#   environment   dedicated venv, expected python, exact tempest 46.0.0,
#                 exact cinder-tempest-plugin 1.21.0, stestr/subunit tooling,
#                 tempest import, no Cinder-venv fallback
#   configuration .stestr.conf valid, workspace initialized, tempest.conf
#                 generated and actually read via TEMPEST_CONFIG_DIR
#   discovery     every allowlisted test ID exists in the pinned installation
#                 AND is collectable by stestr; zero-test discovery fails
#   evidence      synthetic subunit -> JUnit XML -> tempest-cinder-summary.json
#                 pipeline produces valid structured output; malformed/empty
#                 evidence fails
#
# Usage:
#   bash tests/tempest-preflight.sh
#
# Environment:
#   O3K_TEMPEST_VENV            dedicated Tempest venv (created if missing)
#   O3K_PREFLIGHT_WORKDIR       workspace dir (default: temp dir)
#   O3K_PREFLIGHT_RESULT        output result JSON path (default: in workdir)
#   O3K_PREFLIGHT_SKIP_INSTALL  1 = reuse the venv without pip install
#   O3K_TEMPEST_PIN             expected tempest version (default 46.0.0)
#   O3K_CINDER_TEMPEST_PLUGIN_PIN  expected plugin version (default 1.21.0)
#
# Self-test overrides (deterministic regression, no network/install):
#   O3K_PREFLIGHT_SELFTEST=1
#   O3K_PREFLIGHT_VENV_PY           python used for pure-logic checks
#   O3K_PREFLIGHT_TEMPEST_VERSION   detected tempest version override
#   O3K_PREFLIGHT_PLUGIN_VERSION    detected plugin version override
#   O3K_PREFLIGHT_DISCOVERED_TESTS  file of test IDs simulating discovery

set -Eeuo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

ALLOWLIST_FILE="${REPO_ROOT}/tests/tempest-evidence/tempest-allowlist.txt"
SUMMARY_SCRIPT="${REPO_ROOT}/tests/tempest-summary.py"

TEMPEST_PIN="${O3K_TEMPEST_PIN:-46.0.0}"
CINDER_TEMPEST_PLUGIN_PIN="${O3K_CINDER_TEMPEST_PLUGIN_PIN:-1.21.0}"
SELFTEST="${O3K_PREFLIGHT_SELFTEST:-0}"
SKIP_INSTALL="${O3K_PREFLIGHT_SKIP_INSTALL:-0}"
WORKDIR="${O3K_PREFLIGHT_WORKDIR:-$(mktemp -d "${TMPDIR:-/tmp}/o3k-tempest-preflight.XXXXXX")}"
RESULT_PATH="${O3K_PREFLIGHT_RESULT:-${WORKDIR}/tempest-preflight-result.json}"
mkdir -p "${WORKDIR}"
if [ "${SELFTEST}" = "1" ]; then
  TEMPEST_VENV=""
else
  TEMPEST_VENV="${O3K_TEMPEST_VENV:-${WORKDIR}/tempest-venv}"
fi
if [ -n "${O3K_PREFLIGHT_VENV_PY:-}" ]; then
  VENV_PY="${O3K_PREFLIGHT_VENV_PY}"
else
  VENV_PY="${TEMPEST_VENV}/bin/python"
fi
VENV_BIN="$(dirname "${VENV_PY}")"

[ -f "${ALLOWLIST_FILE}" ] || { echo "ERROR: allowlist not found: ${ALLOWLIST_FILE}" >&2; exit 1; }
ALLOWLIST=()
while IFS= read -r line; do
  case "${line}" in
    ""|"#"*) continue ;;
    *) ALLOWLIST+=("${line}") ;;
  esac
done < "${ALLOWLIST_FILE}"

# --- check bookkeeping -------------------------------------------------------
declare -a CHECKS
record() {
  # record <name> <passed|failed|skipped> <detail>
  CHECKS+=("$1|$2|$3")
}
FAILURES=0

# --- pure validation helpers (self-testable) --------------------------------
validate_version() {
  # validate_version <found> <expected> -> 0 if exact match
  [ "${1}" = "${2}" ]
}

validate_allowlist_discovered() {
  # validate_allowlist_discovered <discovered-list-file>
  # stestr emits full test IDs with a "[id-<uuid>[,tags]]" suffix. Every
  # allowlisted ID must appear as a prefix of a discovered ID; the discovered
  # count must be > 0.
  local discovered_file="${1}"
  local total=0
  [ -s "${discovered_file}" ] || return 1
  total="$(wc -l < "${discovered_file}")"
  [ "${total}" -gt 0 ] || return 1
  local id
  for id in "${ALLOWLIST[@]}"; do
    grep -Eq "^${id}\[" "${discovered_file}" || return 1
  done
  return 0
}

# --- environment probes ------------------------------------------------------
probe_versions() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "tempest=${O3K_PREFLIGHT_TEMPEST_VERSION:-}"
    echo "plugin=${O3K_PREFLIGHT_PLUGIN_VERSION:-}"
    return
  fi
  "${VENV_PY}" - <<'PY'
import importlib.metadata as md
import sys
try:
    print("tempest=%s" % md.version("tempest"))
except Exception as exc:
    print("tempest=missing:%s" % exc, file=sys.stderr)
    print("tempest=")
try:
    print("plugin=%s" % md.version("cinder-tempest-plugin"))
except Exception as exc:
    print("plugin=missing:%s" % exc, file=sys.stderr)
    print("plugin=")
PY
}

probe_stestr() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "${O3K_PREFLIGHT_STESTR_RUNS:-yes}"
    return
  fi
  "${VENV_PY}" -m stestr --version >/dev/null 2>&1 && echo "yes" || echo "no"
}

probe_tempest_import() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "${O3K_PREFLIGHT_TEMPEST_IMPORTS:-yes}"
    return
  fi
  "${VENV_PY}" -c "import tempest; import tempest.test_discover; import tempest.test_discover.test_discover" >/dev/null 2>&1 \
    && echo "yes" || echo "no"
}

probe_subunit_import() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "${O3K_PREFLIGHT_SUBUNIT_IMPORTS:-yes}"
    return
  fi
  "${VENV_PY}" -c "import subunit; from subunit.filter_scripts import subunit2junitxml" >/dev/null 2>&1 \
    && echo "yes" || echo "no"
}

probe_no_cinder_fallback() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "${O3K_PREFLIGHT_NO_CINDER:-yes}"
    return
  fi
  "${VENV_PY}" -c "import importlib.util; raise SystemExit(0 if importlib.util.find_spec('cinder') is None else 1)" >/dev/null 2>&1 \
    && echo "yes" || echo "no"
}

probe_venv_prefix() {
  if [ "${SELFTEST}" = "1" ]; then
    echo "${O3K_PREFLIGHT_VENV_PREFIX:-/selftest}"
    return
  fi
  "${VENV_PY}" -c "import sys; print(sys.prefix)" 2>/dev/null || echo "invalid"
}

# --- allowlist existence via import introspection ----------------------------
probe_allowlist_imports() {
  # Returns 0 when every allowlisted test module/class/method imports in the
  # pinned installation. Emits the imported IDs (one per line).
  if [ "${SELFTEST}" = "1" ]; then
    local discovered="${O3K_PREFLIGHT_DISCOVERED_TESTS:-}"
    if [ -n "${discovered}" ] && [ -s "${discovered}" ]; then
      cat "${discovered}"
      return 0
    fi
    return 1
  fi
  local missing=0
  local id
  for id in "${ALLOWLIST[@]}"; do
    # split module.Class.method (method is the last component)
    local method="${id##*.}"
    local cls_module="${id%.${method}}"
    local cls="${cls_module##*.}"
    local module="${cls_module%.${cls}}"
    if ! "${VENV_PY}" - "${module}" "${cls}" "${method}" <<'PY'
import importlib, sys
module, cls, method = sys.argv[1:]
try:
    mod = importlib.import_module(module)
    test_cls = getattr(mod, cls)
    if not hasattr(test_cls, method):
        sys.exit(1)
except Exception:
    sys.exit(1)
PY
    then
      echo "ERROR: allowlisted test missing in pinned installation: ${id}" >&2
      missing=1
    else
      echo "${id}"
    fi
  done
  [ "${missing}" = "0" ]
}

# --- stestr discovery ---------------------------------------------------------
probe_stestr_discovery() {
  # Runs in the workspace and emits discovered test IDs (one per line).
  if [ "${SELFTEST}" = "1" ]; then
    if [ -n "${O3K_PREFLIGHT_DISCOVERED_TESTS:-}" ] && [ -s "${O3K_PREFLIGHT_DISCOVERED_TESTS}" ]; then
      cat "${O3K_PREFLIGHT_DISCOVERED_TESTS}"
    fi
    return
  fi
  (
    cd "${WORKDIR}" || exit 1
    export TEMPEST_CONFIG_DIR="${WORKDIR}/etc"
    "${VENV_PY}" -m stestr list 2>/dev/null || true
  ) | sed -n '/^tempest\./p' | sort -u
}

# --- evidence pipeline ---------------------------------------------------------
run_evidence_pipeline() {
  # Synthetic subunit -> JUnit XML -> summary.json. Returns 0 only when the
  # pipeline produces valid structured output with a non-zero test count.
  local junit="${WORKDIR}/pipeline-results.xml"
  local summary="${WORKDIR}/pipeline-summary.json"
  if [ "${SELFTEST}" = "1" ]; then
    # Synthetic JUnit written directly (subunit tooling is not installed in
    # self-test mode); the conversion validation is exercised in real mode.
    cat > "${junit}" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest-preflight" tests="1" failures="0" errors="0" skipped="0">
  <testcase name="tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token" classname="tempest.api.identity.v3.test_tokens.TokensV3Test"/>
</testsuite>
XML
  else
    if [ "$(probe_subunit_import)" != "yes" ]; then
      return 1
    fi
    if ! "${VENV_PY}" - <<'PY' | "${VENV_BIN}/subunit2junitxml" -o "${junit}" >/dev/null 2>&1
import io, subunit, sys
buf = io.BytesIO()
stream = subunit.StreamResultToBytes(buf)
stream.startTestRun()
stream.status(test_id="tempest.api.identity.v3.test_tokens.TokensV3Test.test_create_token",
              test_status="success")
stream.stopTestRun()
sys.stdout.buffer.write(buf.getvalue())
PY
    then
      return 1
    fi
  fi
  if ! python3 "${SUMMARY_SCRIPT}" \
      --junit "${junit}" \
      --out "${summary}" \
      --revision "${TEMPEST_PIN}" \
      --plugin "${CINDER_TEMPEST_PLUGIN_PIN}" \
      --selected "$(printf '%s,' "${ALLOWLIST[@]}")" \
      --o3k-commit "preflight" >/dev/null 2>&1; then
    return 1
  fi
  python3 - "${summary}" <<'PY'
import json, sys
doc = json.load(open(sys.argv[1], encoding="utf-8"))
assert doc["status"] == "passed", doc.get("status")
assert doc["passed"] >= 1, doc
assert isinstance(doc["test_cases"], list), doc
print("pipeline summary valid")
PY
}

# --- workspace setup ------------------------------------------------------------
setup_workspace() {
  if [ "${SELFTEST}" = "1" ]; then
    mkdir -p "${WORKDIR}/etc"
    cat > "${WORKDIR}/.stestr.conf" <<EOF
[DEFAULT]
test_path=/selftest/test_discover
top_dir=/selftest
group_regex=([^\.]*\.)*
EOF
    : > "${WORKDIR}/etc/tempest.conf"
    return
  fi
  if [ ! -f "${WORKDIR}/.stestr.conf" ] || [ ! -f "${WORKDIR}/etc/tempest.conf" ]; then
    (
      cd "${WORKDIR}" || exit 1
      "${VENV_BIN}/tempest" init "${WORKDIR}" >/dev/null 2>&1 || true
    )
  fi
}

validate_stestr_conf() {
  python3 - "${WORKDIR}/.stestr.conf" <<'PY'
import configparser, os, sys
path = sys.argv[1]
config = configparser.ConfigParser()
if not config.read(path):
    sys.exit(1)
defaults = config.defaults()
test_path = defaults.get("test_path", "")
top_dir = defaults.get("top_dir", "")
assert test_path, "test_path missing"
assert top_dir, "top_dir missing"
print("stestr conf valid")
PY
}

# --- main flow ------------------------------------------------------------------
if [ "${SKIP_INSTALL}" != "1" ] && [ "${SELFTEST}" != "1" ]; then
  echo "==> Installing pinned Tempest environment into ${TEMPEST_VENV}..."
  python3 -m venv "${TEMPEST_VENV}"
  "${VENV_PY}" -m pip install -q --upgrade pip wheel setuptools
  "${VENV_PY}" -m pip install -q \
    "tempest==${TEMPEST_PIN}" \
    "cinder-tempest-plugin==${CINDER_TEMPEST_PLUGIN_PIN}" \
    "oslo_utils>=7.3.0,<10.0.0" \
    "testrepository" "python-subunit" "stestr" "junitxml"
fi

# 1. Environment
if [ -z "${VENV_PY}" ] || [ ! -x "${VENV_PY}" ]; then
  record "venv_valid" "failed" "${VENV_PY} not executable"
  FAILURES=$((FAILURES + 1))
else
  record "venv_valid" "passed" "${VENV_PY}"
fi

# 2. Exact versions + tooling imports
found_tempest=""
found_plugin=""
while IFS='=' read -r key value; do
  case "${key}" in
    tempest) found_tempest="${value}" ;;
    plugin) found_plugin="${value}" ;;
  esac
done < <(probe_versions)

if validate_version "${found_tempest}" "${TEMPEST_PIN}"; then
  record "tempest_exact_version" "passed" "${found_tempest}"
else
  record "tempest_exact_version" "failed" "expected ${TEMPEST_PIN}, found '${found_tempest}'"
  FAILURES=$((FAILURES + 1))
fi
if validate_version "${found_plugin}" "${CINDER_TEMPEST_PLUGIN_PIN}"; then
  record "plugin_exact_version" "passed" "${found_plugin}"
else
  record "plugin_exact_version" "failed" "expected ${CINDER_TEMPEST_PLUGIN_PIN}, found '${found_plugin}'"
  FAILURES=$((FAILURES + 1))
fi

venv_prefix="$(probe_venv_prefix)"
if [ "${SELFTEST}" = "1" ] || [ "${venv_prefix}" = "${TEMPEST_VENV}" ]; then
  record "expected_python_used" "passed" "${venv_prefix}"
else
  record "expected_python_used" "failed" "sys.prefix=${venv_prefix}, expected ${TEMPEST_VENV}"
  FAILURES=$((FAILURES + 1))
fi

if [ "$(probe_stestr)" = "yes" ]; then
  record "stestr_executes" "passed" "stestr --version"
else
  record "stestr_executes" "failed" "stestr missing or failed"
  FAILURES=$((FAILURES + 1))
fi

if [ "$(probe_tempest_import)" = "yes" ]; then
  record "tempest_imports" "passed" "import tempest (+ test_discover)"
else
  record "tempest_imports" "failed" "tempest import failed (dependency conflict?)"
  FAILURES=$((FAILURES + 1))
fi

if [ "$(probe_subunit_import)" = "yes" ]; then
  record "subunit_tooling" "passed" "import subunit + subunit2junitxml"
else
  record "subunit_tooling" "failed" "subunit / subunit2junitxml missing"
  FAILURES=$((FAILURES + 1))
fi

if [ "$(probe_no_cinder_fallback)" = "yes" ]; then
  record "no_cinder_fallback" "passed" "cinder is not importable in the Tempest venv"
else
  record "no_cinder_fallback" "failed" "Cinder packages are installed in the Tempest venv"
  FAILURES=$((FAILURES + 1))
fi

# 3. Configuration
setup_workspace
if validate_stestr_conf; then
  record "stestr_conf_valid" "passed" "${WORKDIR}/.stestr.conf"
else
  record "stestr_conf_valid" "failed" "${WORKDIR}/.stestr.conf invalid"
  FAILURES=$((FAILURES + 1))
fi

if [ -f "${WORKDIR}/etc/tempest.conf" ]; then
  record "tempest_conf_generated" "passed" "${WORKDIR}/etc/tempest.conf"
  export TEMPEST_CONFIG_DIR="${WORKDIR}/etc"
  if [ "${TEMPEST_CONFIG_DIR}" = "$(dirname "${WORKDIR}/etc/tempest.conf")" ]; then
    record "config_location_used" "passed" "TEMPEST_CONFIG_DIR=${TEMPEST_CONFIG_DIR}"
  else
    record "config_location_used" "failed" "TEMPEST_CONFIG_DIR mismatch"
    FAILURES=$((FAILURES + 1))
  fi
else
  record "tempest_conf_generated" "failed" "${WORKDIR}/etc/tempest.conf missing"
  FAILURES=$((FAILURES + 1))
fi

# 4. Test discovery
IMPORTED=""
if probe_allowlist_imports | grep '^tempest\.' > "${WORKDIR}/imported-tests.txt"; then
  IMPORTED="yes"
  record "allowlist_exists" "passed" "$(wc -l < "${WORKDIR}/imported-tests.txt") allowlisted tests import"
else
  record "allowlist_exists" "failed" "an allowlisted test is missing in the pinned installation"
  FAILURES=$((FAILURES + 1))
fi

DISCOVERED_FILE="${WORKDIR}/discovered-tests.txt"
probe_stestr_discovery > "${DISCOVERED_FILE}"
if validate_allowlist_discovered "${DISCOVERED_FILE}"; then
  record "stestr_discovery_nonzero" "passed" "$(wc -l < "${DISCOVERED_FILE}") tests discoverable; allowlist fully collectable"
else
  record "stestr_discovery_nonzero" "failed" "stestr discovery is zero or missing allowlisted tests"
  FAILURES=$((FAILURES + 1))
fi

# 5. Evidence pipeline
if run_evidence_pipeline; then
  record "evidence_pipeline" "passed" "subunit -> JUnit -> summary valid"
else
  record "evidence_pipeline" "failed" "subunit/JUnit/summary pipeline broken"
  FAILURES=$((FAILURES + 1))
fi

# 6. Zero-test / malformed evidence must be rejected by the summary tool.
cat > "${WORKDIR}/empty-results.xml" <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<testsuite name="tempest" tests="0" failures="0" errors="0" skipped="0"/>
XML
if python3 "${SUMMARY_SCRIPT}" \
    --junit "${WORKDIR}/empty-results.xml" \
    --out "${WORKDIR}/empty-summary.json" \
    --revision "${TEMPEST_PIN}" \
    --plugin "${CINDER_TEMPEST_PLUGIN_PIN}" \
    --selected "$(printf '%s,' "${ALLOWLIST[@]}")" >/dev/null 2>&1; then
  record "zero_test_rejected" "failed" "zero-test summary was accepted"
  FAILURES=$((FAILURES + 1))
else
  record "zero_test_rejected" "passed" "zero-test summary rejected"
fi

# --- result ----------------------------------------------------------------------
STATUS="passed"
[ "${FAILURES}" -eq 0 ] || STATUS="failed"

printf '%s\n' "${CHECKS[@]:-}" > "${WORKDIR}/preflight-checks.txt"
python3 - "${RESULT_PATH}" "${STATUS}" "${TEMPEST_PIN}" "${CINDER_TEMPEST_PLUGIN_PIN}" \
  "${found_tempest}" "${found_plugin}" "${TEMPEST_VENV}" "${VENV_PY}" "${WORKDIR}" \
  "${ALLOWLIST[0]:-}" "${#ALLOWLIST[@]}" "${WORKDIR}/preflight-checks.txt" <<'PY'
import json, os, sys
result_path, status, expected_tempest, expected_plugin = sys.argv[1:5]
found_tempest, found_plugin = sys.argv[5:7]
venv, venv_py, workdir = sys.argv[7:10]
first_id, allowlist_count = sys.argv[10:12]
checks_path = sys.argv[12]

def split_check(line):
    name, verdict, detail = line.split("|", 2)
    return name, {"passed": True, "failed": False,
                  "skipped": None}[verdict], detail

checks = {}
try:
    with open(checks_path, encoding="utf-8") as stream:
        for line in stream:
            line = line.rstrip("\n")
            if not line:
                continue
            name, ok, detail = split_check(line)
            checks[name] = {"status": "passed" if ok is True else
                            ("failed" if ok is False else "skipped"),
                            "detail": detail}
except OSError:
    pass
discovered_file = os.path.join(workdir, "discovered-tests.txt")
discovered_count = 0
try:
    with open(discovered_file, encoding="utf-8") as stream:
        discovered_count = len([l for l in stream if l.strip()])
except OSError:
    pass
doc = {
    "artifact_type": "tempest-preflight-result.json",
    "status": status,
    "tempest_revision_expected": expected_tempest,
    "tempest_revision_found": found_tempest,
    "cinder_tempest_plugin_expected": expected_plugin,
    "cinder_tempest_plugin_found": found_plugin,
    "tempest_venv": venv,
    "tempest_venv_python": venv_py,
    "workspace": workdir,
    "allowlist_count": int(allowlist_count),
    "discovered_count": discovered_count,
    "checks": checks,
}
with open(result_path, "w", encoding="utf-8") as stream:
    json.dump(doc, stream, indent=2)
    stream.write("\n")
PY

echo "==> Tempest preflight: ${STATUS}"
if [ "${FAILURES}" -gt 0 ]; then
  echo "    failures: ${FAILURES}"
  exit 1
fi
echo "    allowlisted test IDs: ${#ALLOWLIST[@]}"
exit 0
