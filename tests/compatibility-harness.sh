#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-compatibility-harness.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT
harness="${repo_root}/tests/compatibility-harness.py"

python3 "${harness}" --validate
python3 "${harness}" --self-test
python3 "${harness}" --self-test-mismatch
python3 "${harness}" --self-test-compare
python3 "${harness}" --self-test --json-out "${temp_dir}/self-test.json" --junit-out "${temp_dir}/self-test.xml"
test -s "${temp_dir}/self-test.json"
test -s "${temp_dir}/self-test.xml"
grep -Fq '"passed": true' "${temp_dir}/self-test.json"
grep -Fq '<testsuite' "${temp_dir}/self-test.xml"

python3 - "${repo_root}/docs/compatibility/contract-fixtures.json" "${temp_dir}/missing.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source_file:
    source = json.load(source_file)
source["fixtures"] = source["fixtures"][1:]
with open(sys.argv[2], "w", encoding="utf-8") as output_file:
    json.dump(source, output_file)
PY
if O3K_COMPATIBILITY_FIXTURES="${temp_dir}/missing.json" python3 "${harness}" --validate; then
  echo "incomplete required fixture set was accepted" >&2
  exit 1
fi
