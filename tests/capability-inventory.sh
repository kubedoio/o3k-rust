#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-capability-inventory.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT

python3 "${repo_root}/scripts/generate-capability-inventory.py" \
  --source "${repo_root}/docs/compatibility/capability-inventory-source.json" \
  --json-out "${temp_dir}/capability-inventory.json" \
  --markdown-out "${temp_dir}/capability-inventory.md"
cmp -- "${temp_dir}/capability-inventory.json" "${repo_root}/docs/compatibility/capability-inventory.json"
cmp -- "${temp_dir}/capability-inventory.md" "${repo_root}/docs/compatibility/capability-inventory.md"

grep -Fq '"path": "/v2.1/{project_id}/flavors"' "${repo_root}/docs/compatibility/capability-inventory.json"
grep -Fq '30717871057' "${repo_root}/docs/compatibility/capability-inventory.json"
grep -Fq '"status": 405' "${repo_root}/docs/compatibility/capability-inventory.json"
grep -Fq '"service": "placement"' "${repo_root}/docs/compatibility/capability-inventory.json"

python3 - "${repo_root}/docs/compatibility/capability-inventory-source.json" "${temp_dir}/duplicate.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source_file:
    source = json.load(source_file)
source["operations"].append(dict(source["operations"][0]))
with open(sys.argv[2], "w", encoding="utf-8") as output_file:
    json.dump(source, output_file)
PY
if python3 "${repo_root}/scripts/generate-capability-inventory.py" \
  --source "${temp_dir}/duplicate.json" \
  --json-out "${temp_dir}/duplicate-output.json" \
  --markdown-out "${temp_dir}/duplicate-output.md"; then
  echo "duplicate operation inventory was accepted" >&2
  exit 1
fi

python3 - "${repo_root}/docs/compatibility/capability-inventory-source.json" "${temp_dir}/alias.json" <<'PY'
import json
import sys

with open(sys.argv[1], encoding="utf-8") as source_file:
    source = json.load(source_file)
source["operations"][0]["aliases"] = [source["operations"][0]["path"]]
with open(sys.argv[2], "w", encoding="utf-8") as output_file:
    json.dump(source, output_file)
PY
if python3 "${repo_root}/scripts/generate-capability-inventory.py" \
  --source "${temp_dir}/alias.json" \
  --json-out "${temp_dir}/alias-output.json" \
  --markdown-out "${temp_dir}/alias-output.md"; then
  echo "duplicate route alias was accepted" >&2
  exit 1
fi
