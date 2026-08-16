#!/usr/bin/env python3
"""Validates the Cloud Kernel authorization & service registry inventories.

Contracts:
- contracts/cloud-kernel-actions.yaml
- contracts/cloud-kernel-services.yaml

Fitness functions:
- unique action_id
- non-empty service_namespace and resource_type
- valid target_kind ('collection' or 'instance')
- action_id matches format 'service_namespace:ActionName'
- every registered service action exists in actions inventory
- every action in actions inventory belongs to a registered service
- valid service ownership ('o3k-implemented' or 'external-hosted')
"""

from __future__ import annotations

import argparse
import re
import sys
from pathlib import Path


def check_actions_and_services(root: Path) -> list[str]:
    actions_path = root / "contracts" / "cloud-kernel-actions.yaml"
    services_path = root / "contracts" / "cloud-kernel-services.yaml"

    errors: list[str] = []

    if not actions_path.is_file():
        return [f"missing actions inventory: {actions_path}"]
    if not services_path.is_file():
        return [f"missing services inventory: {services_path}"]

    # 1. Parse actions
    actions_content = actions_path.read_text(encoding="utf-8")
    action_ids: set[str] = set()
    current_action: dict[str, str] = {}

    for line in actions_content.splitlines():
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

    if current_action:
        _validate_action(current_action, action_ids, errors)

    if not action_ids:
        errors.append("cloud-kernel-actions.yaml declares no actions")

    # 2. Parse services
    services_content = services_path.read_text(encoding="utf-8")
    service_ids: set[str] = set()
    service_actions: set[str] = set()
    current_service: dict[str, str | list[str]] = {}
    in_actions_block = False

    for line in services_content.splitlines():
        line_str = line.strip()
        if line_str.startswith("- service_id:"):
            if current_service:
                _validate_service(current_service, service_ids, service_actions, errors)
                current_service = {}
            in_actions_block = False
            val = line_str.split(":", 1)[1].strip().strip('"').strip("'")
            current_service["service_id"] = val
            current_service["actions"] = []
        elif line_str.startswith("actions:"):
            in_actions_block = True
        elif line_str.startswith("api_surfaces:") or line_str.startswith("resource_types:") or line_str.startswith("profiles:"):
            in_actions_block = False
        elif in_actions_block and line_str.startswith("- \"") and (":" in line_str):
            act = line_str.strip("- ").strip('"').strip("'")
            if isinstance(current_service.get("actions"), list):
                current_service["actions"].append(act)
        elif line_str.startswith("ownership:"):
            current_service["ownership"] = line_str.split(":", 1)[1].strip().strip('"').strip("'")
        elif line_str.startswith("namespace:"):
            current_service["namespace"] = line_str.split(":", 1)[1].strip().strip('"').strip("'")

    if current_service:
        _validate_service(current_service, service_ids, service_actions, errors)

    if not service_ids:
        errors.append("cloud-kernel-services.yaml declares no services")

    # 3. Cross-reference actions and services
    for act in sorted(action_ids):
        if act not in service_actions:
            errors.append(f"action {act!r} defined in actions inventory but missing from services inventory")

    for act in sorted(service_actions):
        if act not in action_ids:
            errors.append(f"action {act!r} listed in services inventory but missing from actions inventory")

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
        errors.append(f"{action_id} invalid target_kind: {target_kind!r}")


def _validate_service(service: dict, seen_services: set[str], service_actions: set[str], errors: list[str]) -> None:
    svc_id = str(service.get("service_id", ""))
    if not svc_id:
        errors.append("service missing service_id")
        return

    if svc_id in seen_services:
        errors.append(f"duplicate service_id: {svc_id}")
    seen_services.add(svc_id)

    ownership = service.get("ownership", "")
    if ownership not in ("o3k-implemented", "external-hosted"):
        errors.append(f"service {svc_id} invalid ownership: {ownership!r}")

    actions = service.get("actions", [])
    if isinstance(actions, list):
        for act in actions:
            service_actions.add(str(act))


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--root",
        type=Path,
        default=Path(__file__).resolve().parents[1],
        help="repository root",
    )
    args = parser.parse_args()
    errors = check_actions_and_services(args.root.resolve())
    if errors:
        print("Cloud kernel actions & services validation FAILED:", file=sys.stderr)
        for err in errors:
            print(f"  - {err}", file=sys.stderr)
        return 1
    print("Cloud kernel actions and services validation passed.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
