#!/usr/bin/env python3
"""P13.1A provider-contract evidence foundation."""

from __future__ import annotations

import argparse
import hashlib
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
TOOLCHAIN = ROOT / "docs/compatibility/p13-1/provider-toolchain.json"
SCHEMA = ROOT / "docs/compatibility/p13-1/provider-contract.schema.json"
CORE = {
    "openstack_images_image_v2",
    "openstack_compute_flavor_v2",
    "openstack_compute_keypair_v2",
    "openstack_networking_network_v2",
    "openstack_networking_subnet_v2",
    "openstack_networking_port_v2",
    "openstack_compute_instance_v2",
}
SECRET_NAME = re.compile(r"token|password|secret|private[_-]?key|authorization|cookie", re.I)


def load_json(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def load_trace_records(path: pathlib.Path) -> list[dict[str, Any]]:
    text = path.read_text(encoding="utf-8")
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        value = [json.loads(line) for line in text.splitlines() if line.strip()]
    if isinstance(value, dict):
        value = value.get("traces", value.get("records", []))
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ValueError("raw evidence must be a JSON array/object or JSONL trace file")
    return value


def validate_contract(path: pathlib.Path) -> None:
    document = load_json(path)
    if document.get("schema_version") != 1:
        raise ValueError("unsupported provider-contract schema_version")
    if document.get("artifact_type") != "o3k-p13-1-provider-contract":
        raise ValueError("invalid provider-contract artifact_type")
    for key in ("toolchain", "resources", "traces"):
        if key not in document:
            raise ValueError(f"provider-contract is missing {key}")
    resources = document["resources"]
    if not isinstance(resources, list):
        raise ValueError("resources must be an array")
    names = {item.get("resource") for item in resources if isinstance(item, dict)}
    missing = sorted(CORE - names)
    if missing:
        raise ValueError("missing core resource entries: " + ", ".join(missing))
    forbidden = names - CORE
    if forbidden:
        raise ValueError("P13.1A contains out-of-scope resources: " + ", ".join(sorted(forbidden)))
    for index, trace in enumerate(document["traces"]):
        if not isinstance(trace, dict):
            raise ValueError(f"trace {index} is not an object")
        for key in ("ordinal", "resource", "method", "path", "status"):
            if key not in trace:
                raise ValueError(f"trace {index} is missing {key}")
        if not isinstance(trace["ordinal"], int) or trace["ordinal"] < 0:
            raise ValueError(f"trace {index} has invalid ordinal")
        if trace["resource"] not in CORE and trace["resource"] not in {
            "auth",
            "catalog",
            "extensions",
            "placement",
        }:
            raise ValueError(f"trace {index} has out-of-scope resource")
        if contains_unsanitized_secret(trace):
            raise ValueError(f"trace {index} contains an unsanitized secret")
    ordinals = [trace["ordinal"] for trace in document["traces"]]
    if ordinals != sorted(ordinals):
        raise ValueError("traces must preserve request ordering")


def redact(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: "<redacted>" if SECRET_NAME.search(key) else redact(item) for key, item in value.items()}
    if isinstance(value, list):
        return [redact(item) for item in value]
    return value


def contains_unsanitized_secret(value: Any) -> bool:
    if isinstance(value, dict):
        return any(
            (SECRET_NAME.search(key) and item != "<redacted>")
            or contains_unsanitized_secret(item)
            for key, item in value.items()
        )
    if isinstance(value, list):
        return any(contains_unsanitized_secret(item) for item in value)
    return False


def sha256(path: pathlib.Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def verify_tools() -> None:
    toolchain = load_json(TOOLCHAIN)
    tofu = os.environ.get("O3K_P13_TOFU", "tofu")
    tofu_archive = pathlib.Path(os.environ["O3K_P13_TOFU_ARCHIVE"])
    expected_tofu_archive = toolchain["engine"]["sha256"]
    actual_tofu_archive = sha256(tofu_archive)
    if actual_tofu_archive != expected_tofu_archive:
        raise ValueError(
            f"OpenTofu archive checksum mismatch: expected {expected_tofu_archive}, got {actual_tofu_archive}"
        )
    output = subprocess.check_output([tofu, "version"], text=True, stderr=subprocess.STDOUT)
    if "OpenTofu v1.12.6" not in output:
        raise ValueError("OpenTofu 1.12.6 was not found")
    provider = pathlib.Path(os.environ["O3K_P13_PROVIDER_BINARY"])
    provider_archive = pathlib.Path(os.environ["O3K_P13_PROVIDER_ARCHIVE"])
    expected_provider_archive = toolchain["provider_archive_sha256"]
    actual_provider_archive = sha256(provider_archive)
    if actual_provider_archive != expected_provider_archive:
        raise ValueError(
            f"provider archive checksum mismatch: expected {expected_provider_archive}, got {actual_provider_archive}"
        )
    expected = os.environ.get("O3K_P13_PROVIDER_SHA256")
    if not expected:
        raise ValueError("O3K_P13_PROVIDER_SHA256 is required for the real gate")
    if not provider.is_file() or provider.stat().st_size == 0:
        raise ValueError("provider binary is missing or empty")
    if "3.4.0" not in provider.name:
        raise ValueError("provider binary filename did not identify version 3.4.0")
    actual = sha256(provider)
    manifest_hash = toolchain["provider_sha256"]
    if actual != manifest_hash:
        raise ValueError(f"provider checksum differs from pinned manifest: expected {manifest_hash}, got {actual}")
    if actual != expected:
        raise ValueError(f"provider checksum mismatch: expected {expected}, got {actual}")
    print(json.dumps({"openTofu": "1.12.6", "provider": "3.4.0", "provider_sha256": actual}, sort_keys=True))


def self_test() -> None:
    trace = {"ordinal": 0, "resource": "auth", "method": "POST", "path": "/v3/auth/tokens", "status": 201}
    sanitized = redact({"X-Auth-Token": "secret", "request": trace})
    if sanitized["X-Auth-Token"] != "<redacted>" or sanitized["request"]["status"] != 201:
        raise AssertionError("redaction or wire semantics test failed")
    trace["headers"] = {"Authorization": "secret"}
    safe_trace = redact(trace)
    document = {
        "schema_version": 1,
        "artifact_type": "o3k-p13-1-provider-contract",
        "toolchain": load_json(TOOLCHAIN),
        "resources": [{"resource": resource} for resource in sorted(CORE)],
        "traces": [safe_trace],
    }
    import tempfile

    with tempfile.TemporaryDirectory() as directory:
        path = pathlib.Path(directory) / "provider-contract.json"
        path.write_text(json.dumps(document), encoding="utf-8")
        validate_contract(path)
    print("P13.1A evidence self-test passed")


def run_real() -> None:
    project = pathlib.Path(os.environ["O3K_P13_TOFU_PROJECT"])
    output = pathlib.Path(os.environ["O3K_P13_EVIDENCE_OUTPUT"])
    if not project.is_dir():
        raise ValueError("O3K_P13_TOFU_PROJECT must be an existing OpenTofu project")
    tofu = os.environ.get("O3K_P13_TOFU", "tofu")
    env = os.environ.copy()
    env["TF_IN_AUTOMATION"] = "1"
    with tempfile.TemporaryDirectory(prefix="o3k-p13-tofu-") as directory:
        disposable_project = pathlib.Path(directory) / "project"
        shutil.copytree(project, disposable_project)
        subprocess.run(
            [tofu, "init", "-input=false", "-upgrade=false"],
            cwd=disposable_project,
            env=env,
            check=True,
        )
        subprocess.run(
            [tofu, "plan", "-input=false", "-refresh=false"],
            cwd=disposable_project,
            env=env,
            check=True,
        )
    raw_path = pathlib.Path(os.environ["O3K_P13_RAW_EVIDENCE"])
    traces = redact(load_trace_records(raw_path))
    document = {
        "schema_version": 1,
        "artifact_type": "o3k-p13-1-provider-contract",
        "toolchain": load_json(TOOLCHAIN),
        "resources": [{"resource": resource, "evidence": "observed-or-deferred"} for resource in sorted(CORE)],
        "traces": traces,
    }
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    validate_contract(output)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--validate", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--verify-tools", action="store_true")
    parser.add_argument("--run-real", action="store_true")
    args = parser.parse_args()
    try:
        if args.validate:
            load_json(TOOLCHAIN)
            load_json(SCHEMA)
            print("P13.1A schemas and toolchain manifest are valid")
        if args.self_test:
            self_test()
        if args.verify_tools:
            verify_tools()
        if args.run_real:
            run_real()
        if not any(vars(args).values()):
            parser.error("one action is required")
    except (OSError, subprocess.CalledProcessError, ValueError, AssertionError, KeyError) as error:
        print(f"P13.1A failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
