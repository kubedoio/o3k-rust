#!/usr/bin/env python3
"""Validate the sanitized, discovery-only P13.3A SG artifact."""

from __future__ import annotations

import json
import pathlib
import sys
from typing import Any


ROOT = pathlib.Path(__file__).resolve().parents[1]
ARTIFACT = ROOT / "docs/compatibility/p13-3/p13-3a-security-group-provider-contract.json"
SECRET_KEYS = {"authorization", "x-auth-token", "x-subject-token", "cookie"}
SECRET_NAME_PARTS = ("password", "private_key", "private-key", "token-signing-key")


def walk(value: Any, path: str = ""):
    if isinstance(value, dict):
        for key, item in value.items():
            yield path + "." + key, key.lower(), item
            yield from walk(item, path + "." + key)
    elif isinstance(value, list):
        for index, item in enumerate(value):
            yield from walk(item, f"{path}[{index}]")


def main() -> int:
    document = json.loads(ARTIFACT.read_text(encoding="utf-8"))
    assert document["artifact_type"] == "o3k-p13-3a-security-group-provider-contract"
    assert document["phase"] == "p13.3a"
    assert document["status"] == "discovery_complete_architecture_gate_blocked"
    assert document["toolchain"] == {
        "opentofu": "1.12.6",
        "opentofu_archive_sha256": "50a6106fa4de523d09c87af85f3db1dd47535fc005727fdca6852146476b88ec",
        "provider": "terraform-provider-openstack/openstack 3.4.0",
        "provider_archive_sha256": "11b3c88e24197a29b13cf5ab41771944bd16707b561645323e8cbb4f1da00b7b",
        "provider_binary_sha256": "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc",
        "provider_sha256_expected": "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc",
        "provider_modified": False,
    }
    observed = document["observed_execution"]
    assert observed["apply_exit_status"] == 0
    assert observed["destroy_exit_status"] == 0
    trace = observed["trace"]
    assert trace
    paths = [(item["method"], item["path"], item["status"]) for item in trace]
    assert any(method == "POST" and path == "/v2.0/security-groups" and status == 201 for method, path, status in paths)
    assert any(method == "GET" and path.startswith("/v2.0/security-groups/") and status == 200 for method, path, status in paths)
    assert any(method == "POST" and path == "/v2.0/security-group-rules" and status == 201 for method, path, status in paths)
    assert any(method == "GET" and path.startswith("/v2.0/security-group-rules/") and status == 200 for method, path, status in paths)
    assert any(method == "DELETE" and path.startswith("/v2.0/security-group-rules/") and status == 204 for method, path, status in paths)
    assert any(method == "DELETE" and path.startswith("/v2.0/security-groups/") and status == 204 for method, path, status in paths)
    assert any(method == "GET" and path.startswith("/v2.0/security-group-rules/") and status == 404 for method, path, status in paths)
    assert any(method == "GET" and path.startswith("/v2.0/security-groups/") and status == 404 for method, path, status in paths)
    for path, key, value in walk(document):
        if key in SECRET_KEYS or any(part in key for part in SECRET_NAME_PARTS):
            assert value == "<redacted>" or value is True or value == "ignored temporary output", f"secret at {path}"
    mapping = document["canonical_mapping_review"]
    assert mapping["current_fit"] == "blocked_pending_proposed_architecture_acceptance"
    proposal = mapping["proposed_architecture"]
    assert proposal["status"] == "proposed_not_accepted"
    assert proposal["runtime_authorized"] is False
    assert proposal["adr"].endswith("ADR-0177-canonical-networkpolicy-and-reusable-policy-set.md")
    assert proposal["spec"].endswith("SPEC-0034-canonical-networkpolicy-lifecycle-v1.md")
    assert any(item["severity"] == "BLOCKER" for item in document["architecture_findings"])
    print(f"P13.3A discovery artifact valid: {ARTIFACT}")
    print(f"trace records: {len(trace)}")
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (AssertionError, KeyError, json.JSONDecodeError) as error:
        print(f"P13.3A discovery artifact invalid: {error}", file=sys.stderr)
        raise SystemExit(1)
