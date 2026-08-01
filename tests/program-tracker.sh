#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-program-tracker.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" \
  --input "${ROOT_DIR}/docs/release-tracker.md"

python3 - "${ROOT_DIR}/docs/release-tracker.md" "${WORK_DIR}/ready.md" "${WORK_DIR}/claimed.md" "${WORK_DIR}/release-ready.md" "${WORK_DIR}/missing-row.md" "${WORK_DIR}/missing-marker.md" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
pathlib.Path(sys.argv[2]).write_text(
    source.replace("program_status: blocked", "program_status: ready"),
    encoding="utf-8",
)
pathlib.Path(sys.argv[3]).write_text(
    source.replace("no program closure is claimed", "program closure is claimed"),
    encoding="utf-8",
)
pathlib.Path(sys.argv[4]).write_text(
    source.replace("blocked: real host evidence", "release-ready: true"),
    encoding="utf-8",
)
pathlib.Path(sys.argv[5]).write_text(
    "\n".join(line for line in source.splitlines() if not line.startswith("| #86 |")) + "\n",
    encoding="utf-8",
)
pathlib.Path(sys.argv[6]).write_text(
    source.replace("closure evidence: pending", "closure evidence: passed", 1),
    encoding="utf-8",
)
PY

if bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" --input "${WORK_DIR}/ready.md"; then
  echo "accepted ready program status" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" --input "${WORK_DIR}/claimed.md"; then
  echo "accepted program closure claim" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" --input "${WORK_DIR}/release-ready.md"; then
  echo "accepted release-ready claim" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" --input "${WORK_DIR}/missing-row.md"; then
  echo "accepted tracker with a missing issue #94 closure row" >&2
  exit 1
fi
if bash "${ROOT_DIR}/packaging/validate-program-tracker.sh" --input "${WORK_DIR}/missing-marker.md"; then
  echo "accepted tracker with a positive closure-evidence marker" >&2
  exit 1
fi

echo "program tracker contract tests passed"
