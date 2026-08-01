#!/usr/bin/env python3
"""Generate the deterministic TestLab compatibility capability inventory."""

from __future__ import annotations

import argparse
import json
import pathlib
import sys
from typing import Any, NoReturn

METHODS = {"DELETE", "GET", "POST", "PUT"}
STATES = {"implemented", "partial", "missing", "unsupported"}
EVIDENCE = {"verified", "pending", "failed", "not-applicable"}
SERVICES = {"identity", "image", "network", "compute", "placement"}


def fail(message: str) -> NoReturn:
    raise ValueError(message)


def load_source(path: pathlib.Path) -> dict[str, Any]:
    try:
        source = json.loads(path.read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        fail(f"cannot read inventory source {path}: {error}")
    if not isinstance(source, dict):
        fail("inventory source must be a JSON object")

    operations = source.get("operations")
    if not isinstance(operations, list) or not operations:
        fail("inventory source must contain a non-empty operations array")
    if set(source.get("required_services", [])) != SERVICES:
        fail("required_services must list identity, image, network, compute, and placement")

    seen_ids: set[str] = set()
    seen_routes: set[tuple[str, str]] = set()
    seen_aliases: set[tuple[str, str]] = set()
    normalized: list[dict[str, Any]] = []
    for entry in operations:
        if not isinstance(entry, dict):
            fail("every operation must be an object")
        required = {
            "id",
            "service",
            "operation",
            "method",
            "path",
            "official_sources",
            "go_locations",
            "rust_locations",
            "cli_command",
            "implementation_state",
            "portable_contract",
            "cli_verification",
            "protected_runner",
            "release_relevance",
        }
        missing = required - set(entry)
        if missing:
            fail(f"{entry.get('id', '<unknown>')} is missing {sorted(missing)}")
        operation_id = entry["id"]
        if not isinstance(operation_id, str) or not operation_id:
            fail("operation id must be a non-empty string")
        if operation_id in seen_ids:
            fail(f"duplicate operation id: {operation_id}")
        seen_ids.add(operation_id)
        service = entry["service"]
        if service not in SERVICES:
            fail(f"{operation_id} uses unknown service {service!r}")
        method = entry["method"]
        path_value = entry["path"]
        route = (method, path_value)
        if method not in METHODS or not isinstance(path_value, str) or not path_value.startswith("/"):
            fail(f"{operation_id} has an invalid method or path")
        if route in seen_routes or route in seen_aliases:
            fail(f"duplicate method/path route: {method} {path_value}")
        seen_routes.add(route)
        aliases = entry.setdefault("aliases", [])
        if not isinstance(aliases, list) or not all(isinstance(alias, str) for alias in aliases):
            fail(f"{operation_id}.aliases must be a string array")
        for alias in aliases:
            alias_route = (method, alias)
            if not alias.startswith("/") or alias_route in seen_routes or alias_route in seen_aliases:
                fail(f"{operation_id} has a duplicate or invalid alias {alias!r}")
            seen_aliases.add(alias_route)
        observed = entry.get("observed_method_status")
        if observed is not None:
            if not isinstance(observed, dict) or observed.get("method") not in METHODS:
                fail(f"{operation_id} has an invalid observed method")
            if not isinstance(observed.get("status"), int) or not 100 <= observed["status"] <= 599:
                fail(f"{operation_id} has an invalid observed status")
        if not entry["official_sources"] or any(
            not isinstance(url, str) or not url.startswith("https://docs.openstack.org/")
            for url in entry["official_sources"]
        ):
            fail(f"{operation_id} must cite official OpenStack documentation")
        for field in ("go_locations", "rust_locations"):
            if not isinstance(entry[field], list) or not all(isinstance(value, str) for value in entry[field]):
                fail(f"{operation_id}.{field} must be a string array")
        if entry["implementation_state"] not in STATES:
            fail(f"{operation_id} has invalid implementation state")
        for field in ("portable_contract", "cli_verification", "protected_runner"):
            if entry[field] not in EVIDENCE:
                fail(f"{operation_id} has invalid {field} state")
        if entry["release_relevance"] not in {"required", "supporting", "out-of-scope"}:
            fail(f"{operation_id} has invalid release relevance")
        normalized.append(dict(entry))

    if {entry["service"] for entry in normalized} != SERVICES:
        fail("operations do not cover every required service")
    source["operations"] = sorted(
        normalized, key=lambda entry: (entry["service"], entry["path"], entry["method"], entry["id"])
    )
    source["operation_count"] = len(normalized)
    return source


def markdown(source: dict[str, Any]) -> str:
    lines = [
        "# TestLab capability inventory",
        "",
        "This file is generated from `capability-inventory-source.json`; edit the source and rerun the generator.",
        "",
        f"- Profile: `{source['profile']}`",
        f"- Go O3K reference: `{source['go_reference']['commit']}`",
        f"- Rust reference: `{source['rust_reference']['commit']}`",
        f"- Operations: `{source['operation_count']}`",
        "",
        "Evidence states are independent: route implementation does not imply contract, CLI, or protected-runner verification.",
        "",
        "| Service | Operation | Method | Canonical path | Implementation | Contract | CLI | Protected runner | Relevance |",
        "| --- | --- | --- | --- | --- | --- | --- | --- | --- |",
    ]
    for entry in source["operations"]:
        values = [
            entry["service"],
            entry["operation"],
            entry["method"],
            entry["path"],
            entry["implementation_state"],
            entry["portable_contract"],
            entry["cli_verification"],
            entry["protected_runner"],
            entry["release_relevance"],
        ]
        lines.append("| " + " | ".join(value.replace("|", "\\|") for value in values) + " |")
    lines.extend(["", "## Known gaps", ""])
    for entry in source["operations"]:
        for gap in entry.get("known_gaps", []):
            lines.append(f"- `{entry['id']}`: {gap}")
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--source", type=pathlib.Path, required=True)
    parser.add_argument("--json-out", type=pathlib.Path, required=True)
    parser.add_argument("--markdown-out", type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        source = load_source(args.source)
        args.json_out.parent.mkdir(parents=True, exist_ok=True)
        args.markdown_out.parent.mkdir(parents=True, exist_ok=True)
        args.json_out.write_text(json.dumps(source, indent=2, sort_keys=True) + "\n", encoding="utf-8")
        args.markdown_out.write_text(markdown(source), encoding="utf-8")
    except (OSError, ValueError) as error:
        print(f"capability inventory: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
