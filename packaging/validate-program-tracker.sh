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

REQUIRED_CLOSURE_ISSUES = {
    76, 77, 78, 79, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93, 94
}
CLOSURE_PENDING_MARKER = re.compile(r"closure evidence:\s*([a-z-]+)")

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

for issue in REQUIRED_CLOSURE_ISSUES:
    if issue not in rows:
        errors.append(f"required issue #94 closure row #{issue} is missing")
        continue
    evidence = " ".join(rows[issue][3].lower().split())
    markers = CLOSURE_PENDING_MARKER.findall(evidence)
    if markers != ["pending"]:
        errors.append(
            f"issue #{issue} closure row must contain exactly 'closure evidence: pending'"
        )

row_93 = rows.get(93)
if row_93:
    value = " | ".join(row_93).lower()
    if "blocked" not in value or "no release-ready" not in value:
        errors.append("issue #93 row must remain explicitly blocked and non-claiming")
    if any(word in value for word in ("release-ready: true", "published: true", "signed: true")):
        errors.append("issue #93 row contains a forbidden positive release claim")

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
