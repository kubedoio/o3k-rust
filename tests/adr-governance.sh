#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
adr_root="${O3K_ADR_ROOT:-${repo_root}/docs/adr}"

python3 - "${adr_root}" <<'PY'
import pathlib
import re
import sys

root = pathlib.Path(sys.argv[1])
allowed = {"draft", "proposed", "accepted", "rejected", "superseded"}
files = sorted(root.glob("ADR-*.md"))
if not files:
    raise SystemExit(f"no ADR files found under {root}")

records = {}
for path in files:
    match = re.fullmatch(r"ADR-(\d{4})-.+\.md", path.name)
    if not match:
        raise SystemExit(f"invalid ADR filename: {path.name}")
    identifier = f"ADR-{match.group(1)}"
    if identifier in records:
        raise SystemExit(f"duplicate ADR identifier: {identifier}")
    text = path.read_text(encoding="utf-8")
    heading = re.search(r"^#\s+(ADR-\d{4})\b", text, re.MULTILINE)
    if not heading or heading.group(1) != identifier:
        raise SystemExit(f"{path}: heading must identify {identifier}")

    status_match = re.search(r"^Status:\s*(\S+)", text, re.MULTILINE | re.IGNORECASE)
    if status_match:
        status = status_match.group(1).rstrip(".").lower()
    else:
        section = re.search(r"^##\s+Status\s*$([\s\S]*?)(?=^##\s+|\Z)", text, re.MULTILINE | re.IGNORECASE)
        if not section:
            raise SystemExit(f"{path}: missing Status metadata")
        line = next((line.strip() for line in section.group(1).splitlines() if line.strip()), "")
        status = line.split()[0].rstrip(".").lower() if line else ""
    if status not in allowed:
        raise SystemExit(f"{path}: invalid ADR status {status!r}")

    supersedes = []
    for field in ("Supersedes", "Superseded-by"):
        field_match = re.search(rf"^{field}:\s*(.+)$", text, re.MULTILINE | re.IGNORECASE)
        if field_match and field_match.group(1).strip().lower() != "none":
            supersedes.extend(re.findall(r"ADR-\d{4}", field_match.group(1)))
    records[identifier] = {"path": path, "status": status, "links": supersedes}

for identifier, record in records.items():
    for target in record["links"]:
        if target not in records:
            raise SystemExit(f"{record['path']}: dangling ADR link {target}")

def visit(identifier, stack):
    if identifier in stack:
        cycle = " -> ".join(stack[stack.index(identifier):] + [identifier])
        raise SystemExit(f"ADR supersession cycle: {cycle}")
    if identifier in visited:
        return
    stack.append(identifier)
    for target in records[identifier]["links"]:
        visit(target, stack)
    stack.pop()
    visited.add(identifier)

visited = set()
for identifier in records:
    visit(identifier, [])

print(f"validated ADR governance: {len(records)} ADRs, allowed statuses, resolved acyclic links")
PY

temp_root="$(mktemp -d "${TMPDIR:-/tmp}/o3k-adr-governance.XXXXXX")"
trap 'rm -rf "${temp_root}"' EXIT
cp "${adr_root}/ADR-0153-static-rust-and-openstack-release-policy.md" "${temp_root}/ADR-0153-static-rust-and-openstack-release-policy.md"
sed -i 's/^Status: Accepted$/Status: Invalid/' "${temp_root}/ADR-0153-static-rust-and-openstack-release-policy.md"
if O3K_ADR_ROOT="${temp_root}" bash "${BASH_SOURCE[0]}" >/dev/null 2>&1; then
    echo "ADR validator accepted an invalid status" >&2
    exit 1
fi
sed -i 's/^Status: Invalid$/Status: Proposed\nSupersedes: ADR-9999/' "${temp_root}/ADR-0153-static-rust-and-openstack-release-policy.md"
if O3K_ADR_ROOT="${temp_root}" bash "${BASH_SOURCE[0]}" >/dev/null 2>&1; then
    echo "ADR validator accepted a dangling supersession link" >&2
    exit 1
fi
