#!/usr/bin/env python3
"""Validate the committed, sanitized P13.2D server evidence."""

import json
import re
import sys
from pathlib import Path


EXPECTED_TOFU = "50a6106fa4de523d09c87af85f3db1dd47535fc005727fdca6852146476b88ec"
EXPECTED_PROVIDER = "2840ef5e25598f85591cf984825a8a19b9de498782cfe253e6d3e78740fbd5dc"
EXPECTED_PROVIDER_ARCHIVE = "11b3c88e24197a29b13cf5ab41771944bd16707b561645323e8cbb4f1da00b7b"
SECRET_KEY = re.compile(
    r"(?:x-auth-token|x-subject-token|authorization|password|private[_-]?key|"
    r"token[_-]?signing|database[_-]?url)", re.I
)


def walk(value):
    if isinstance(value, dict):
        for key, item in value.items():
            if SECRET_KEY.search(key) and item != "<redacted>":
                raise AssertionError(f"unredacted secret field: {key}")
            yield from walk(item)
    elif isinstance(value, list):
        for item in value:
            yield from walk(item)
    elif isinstance(value, str):
        if "BEGIN " in value or "Bearer " in value:
            raise AssertionError("secret-like value in evidence")


def main(path: Path) -> None:
    document = json.loads(path.read_text(encoding="utf-8"))
    assert document["phase"] == "P13.2D"
    assert document["redacted"] is True
    assert document["raw_trace_committed"] is False
    assert document["secrets_redacted"] is True
    assert re.fullmatch(r"[0-9a-f]{40}", document["tested_implementation_sha"])
    toolchain = document["toolchain"]
    assert toolchain["opentofu"] == "1.12.6"
    assert toolchain["opentofu_archive_sha256"] == EXPECTED_TOFU
    assert toolchain["provider"] == "terraform-provider-openstack/openstack 3.4.0"
    assert toolchain["provider_archive_sha256"] == EXPECTED_PROVIDER_ARCHIVE
    assert toolchain["provider_binary_sha256"] == EXPECTED_PROVIDER
    assert toolchain["provider_modified"] is False
    runs = {run["backend"]: run for run in document["runs"]}
    assert set(runs) == {"sqlite", "postgres"}
    for backend, run in runs.items():
        identities = run["identities"]
        for key in ("server_id", "endpoint_id", "network_id", "realm_id", "subnet_id", "fixed_ip", "mac_address"):
            assert identities[key]
        assert identities["realm_id"] == identities["subnet_id"]
        identity = run["trace_client_identity"]
        assert identity["execution_engine"] == "OpenTofu 1.12.6"
        assert any("Terraform Provider OpenStack/3.4.0" in agent and "gophercloud/v2.8.0" in agent for agent in identity["provider_user_agents"])
        assert run["ownership"] == {
            "endpoint_realm_id_equals_subnet_id": True,
            "realm_network_id_equals_network_id": True,
            "server_owned_endpoint": True,
        }
        trace = run["http_trace"]
        assert trace and all(item["sequence"] == i for i, item in enumerate(trace))
        assert any(item["method"] == "POST" and "/servers" in item["path"] and item["status"] == 202 for item in trace)
        assert any(item["method"] == "DELETE" and "/servers/" in item["path"] and item["status"] == 204 for item in trace)
        assert run["cleanup"] == {"endpoint_status": 404, "network_status": 200, "subnet_status": 200}
        list(walk(trace))
    print(f"validated P13.2D evidence: {path} ({', '.join(sorted(runs))})")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: p13_2d_evidence_validation.py EVIDENCE.json")
    main(Path(sys.argv[1]))
