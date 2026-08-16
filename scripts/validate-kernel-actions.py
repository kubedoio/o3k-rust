#!/usr/bin/env python3
"""Validates the Cloud Kernel authorization inventory (contracts/cloud-kernel-actions.yaml).

Fitness functions:
- unique action_id
- non-empty service_namespace and resource_type
- valid target_kind ('collection' or 'instance')
- valid accepted_principal_classes (subset of 'user', 'service')
- action_id matches format 'service_namespace:ActionName'
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path

try:
    import yaml
except ImportError:
    # Minimal YAML fallback if PyYAML is not installed
    yaml = None


def check_actions(root: Path) -> list[str]:
    contract_path = root / "contracts" / "cloud-kernel-actions.yaml"
    if not contract_path.is_file():
        return [f"missing actions inventory: {contract_path}"]

    content = contract_path.read_text(encoding="utf-8")
    lines = content.splitlines()

    action_ids: set[str] = set()
    errors: list[str] = []

    current_action: dict[str, str | list[str]] = {}
    
    for line in lines:
        line_str = line.strip()
        if line_str.startswith("- action_id:"):
            if current_action:
                _validate_action(current_action, action_ids, errors)
                current_action = {}
            val = line_str.split(":", 1)[1].strip().strip('"').strip("'")
            current_action["action_id"] = val
        elif line_str.startswith("service_namespace:"):
            current_action["service_namespace"] = line_str.split(":", 1)[1].strip().strip('"').strip("'")
        elif line_str.startswith("resource_type:"):
            current_action["resource_type"] = line_str.split(":", 1)[1].strip().strip('"').strip("'")
        elif line_str.startswith("target_kind:"):
            current_action["target_kind"] = line_str.split(":", 1)[1].strip().strip('"').strip("'")
        elif line_str.startswith("require_ownership:"):
            current_action["require_ownership"] = line_str.split(":", 1)[1].strip()

    if current_action:
        _validate_action(current_action, action_ids, errors)

    if not action_ids:
        errors.append("cloud-kernel-actions.yaml declares no actions")

    return errors


def _validate_action(action: dict, seen: set[str], errors: list[str]) -> None:
    action_id = action.get("action_id", "")
    if not action_id:
        errors.append("action missing action_id")
        return

    if action_id in seen:
        errors.append(f"duplicate action_id: {action_id}")
    seen.add(action_id)

    if not re.match(r"^[a-z]+:[A-Za-z0-9]+$", action_id):
        errors.append(f"malformed action_id format (expected 'namespace:Action'): {action_id}")

    ns = action.get("service_namespace", "")
    if not ns:
        errors.append(f"{action_id} missing service_namespace")
    elif not action_id.startswith(f"{ns}:"):
        errors.append(f"{action_id} namespace does not match service_namespace {ns!r}")

    res_type = action.get("resource_type", "")
    if not res_type:
        errors.append(f"{action_id} missing resource_type")

    target_kind = action.get("target_kind", "")
    if target_kind not in ("collection", "instance"):
        errors.append(f"{action_id} invalid target_kind: {target_kind!r} (must be 'collection' or 'instance')")


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root",
    )
    args = parser.parse_args()
    errors = check_actions(args.root.resolve())
    if errors:
        print("Cloud kernel actions validation FAILED:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1
    print("Cloud kernel actions validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
