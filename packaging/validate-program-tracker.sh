#!/usr/bin/env bash
set -Eeuo pipefail

INPUT=""
while (($#)); do
  case "$1" in
    --input) INPUT="${2:?missing tracker path}"; shift 2;;
    *) echo "unknown option: $1" >&2; exit 2;;
  esac
done

if [[ -z "$INPUT" ]]; then
  echo "--input is required" >&2
  exit 2
fi

export INPUT
python3 <<'PY'
import os
import re
import sys

path = os.environ["INPUT"]
try:
    with open(path, encoding="utf-8") as stream:
        text = stream.read()
except OSError as error:
    print(f"program-tracker: cannot read tracker ({error})", file=sys.stderr)
    raise SystemExit(1)

errors = []
metadata_match = re.search(
    r"<!-- tracker-contract\n(?P<body>.*?)\n-->", text, re.DOTALL
)
if not metadata_match:
    errors.append("tracker-contract metadata block is required")
else:
    metadata = {}
    for line in metadata_match.group("body").splitlines():
        if ":" not in line:
            errors.append("tracker-contract metadata lines must use key: value")
            continue
        key, value = line.split(":", 1)
        metadata[key.strip()] = value.strip()
    expected = {
        "owner_issue": "94",
        "release_issue": "93",
        "program_status": "blocked",
        "closure_decision": "pending",
    }
    for key, value in expected.items():
        if metadata.get(key) != value:
            errors.append(f"tracker-contract {key} must remain {value!r}")

if "## Decision log" not in text:
    errors.append("decision log heading is required")
if "## Evidence required to close the program" not in text:
    errors.append("evidence requirements heading is required")
if "ADR-0100" not in text:
    errors.append("ADR-0100 must be linked from the tracker decision log")
if "issue #93 owns release-gate execution and publication" not in text:
    errors.append("tracker must keep release-gate ownership with issue #93")

rows = {}
for line in text.splitlines():
    if not line.startswith("|") or not re.match(r"\| #\d+ \|", line):
        continue
    cells = [cell.strip() for cell in line.strip().strip("|").split("|")]
    if len(cells) != 4:
        errors.append("every issue row must have four table cells")
        continue
    match = re.fullmatch(r"#(\d+)", cells[0])
    if not match:
        errors.append(f"invalid issue row identifier: {cells[0]!r}")
        continue
    issue = int(match.group(1))
    if issue in rows:
        errors.append(f"issue #{issue} appears more than once")
    rows[issue] = cells

for issue in (94,):
    if issue not in rows:
        errors.append(f"required closure row #{issue} is missing")

row_94 = rows.get(94)
if row_94:
    value = " | ".join(row_94).lower()
    if "blocked" not in value or "no program closure" not in value:
        errors.append("issue #94 row must remain explicitly blocked and non-claiming")

if errors:
    for error in errors:
        print(f"program-tracker: {error}", file=sys.stderr)
    raise SystemExit(1)
print("validated program tracker: closure remains blocked and pending")
PY
