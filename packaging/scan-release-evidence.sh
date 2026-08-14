#!/usr/bin/env bash
set -Eeuo pipefail

# Scan a release/review package before publication.  This intentionally does
# not attempt to prove that a value is safe; it rejects the two classes of
# material that must never be published and leaves classification to a human.
if (($# == 0)); then
  echo "usage: $0 PATH [PATH ...]" >&2
  exit 2
fi

export SCAN_PATHS="$*"
python3 - "$@" <<'PY'
from __future__ import annotations

import re
import sys
from pathlib import Path

paths = [Path(value) for value in sys.argv[1:]]
errors: list[str] = []
seen: set[Path] = set()

# PEM private keys are unambiguous and are rejected even when the enclosing
# file is otherwise binary or has an unfamiliar extension.
private_key = re.compile(
    rb"-----BEGIN (?:OPENSSH|RSA|EC|DSA|PGP|ENCRYPTED)? ?PRIVATE KEY-----"
)
assignment = re.compile(
    r"(?im)\b(password|passwd|token(?:[_-](?:signing[_-]?)?key)?|"
    r"signing[_-]?key|private[_-]?key|secret|credential|os[_-]?password)"
    r"(?![A-Za-z0-9_])[\"']?\s*[:=]\s*"
    r"(?:\"([^\"]*)\"|'([^']*)'|([^\s,}\]]+))"
)
safe_values = {
    "",
    "none",
    "null",
    "false",
    "true",
    "redacted",
    "<redacted>",
    "<secret>",
    "<password>",
    "<token>",
    "<value>",
    "placeholder",
    "example",
    "example-secret",
    "changeme",
    "change-me",
    "***",
    "xxxxx",
}


def files_under(path: Path):
    if path.is_symlink():
        errors.append(f"symlink is not scannable: {path}")
        return
    if path.is_file():
        yield path
    elif path.is_dir():
        for child in sorted(path.rglob("*")):
            if child.is_symlink():
                errors.append(f"symlink is not scannable: {child}")
            elif child.is_file():
                yield child
    else:
        errors.append(f"path does not exist or is not a regular file/directory: {path}")


for root in paths:
    for path in files_under(root):
        try:
            resolved = path.resolve()
            if resolved in seen:
                continue
            seen.add(resolved)
            data = path.read_bytes()
        except OSError as error:
            errors.append(f"cannot read {path}: {error}")
            continue
        for match in private_key.finditer(data):
            line = data[: match.start()].count(b"\n") + 1
            errors.append(f"{path}:{line}: private-key PEM material")
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError:
            # Binary release executables cannot contain a PEM header and are
            # not useful inputs for the textual assignment scan.
            continue
        for match in assignment.finditer(text):
            value = next(group for group in match.groups()[1:] if group is not None)
            normalized = value.strip().lower()
            if normalized in safe_values or normalized.startswith(("${{", "$env:", "$")):
                continue
            line = text.count("\n", 0, match.start()) + 1
            errors.append(f"{path}:{line}: secret-bearing {match.group(1).lower()} assignment")

if errors:
    for error in sorted(set(errors)):
        print(f"evidence-secret-scan: {error}", file=sys.stderr)
    raise SystemExit(1)
print(f"evidence-secret-scan: scanned {len(seen)} file(s); no private keys or secret assignments found")
PY
