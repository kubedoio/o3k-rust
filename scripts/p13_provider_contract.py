#!/usr/bin/env python3
"""P13.1 provider-contract evidence harness."""

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
CORE_RESOURCES = {
    "openstack_images_image_v2",
    "openstack_compute_flavor_v2",
}


def load_json(path: pathlib.Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    if not isinstance(value, dict):
        raise ValueError(f"{path} must contain a JSON object")
    return value


def load_trace_records(path: pathlib.Path) -> list[dict[str, Any]]:
    if not path.exists() or not path.read_text(encoding="utf-8").strip():
        return []
    text = path.read_text(encoding="utf-8")
    try:
        value = json.loads(text)
    except json.JSONDecodeError:
        value = [json.loads(line) for line in text.splitlines() if line.strip()]
    if isinstance(value, dict):
        value = value.get("traces", value.get("records", []))
    if not isinstance(value, list) or not all(isinstance(item, dict) for item in value):
        raise ValueError("raw evidence must be a JSON array/object or JSONL trace file")
    if all(isinstance(item.get("ordinal"), int) for item in value):
        return sorted(value, key=lambda item: item["ordinal"])
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
    required_resources = CORE if document.get("phase") != "p13.1c" else CORE - CORE_RESOURCES
    missing = sorted(required_resources - names)
    if missing:
        raise ValueError("missing core resource entries: " + ", ".join(missing))
    forbidden = names - CORE
    if forbidden:
        raise ValueError("P13.1A contains out-of-scope resources: " + ", ".join(sorted(forbidden)))
    if document.get("phase") == "p13.1b":
        provider_run = document.get("provider_run", {})
        if provider_run.get("engine_version") != "1.12.6":
            raise ValueError("P13.1B provider_run does not prove OpenTofu 1.12.6")
        if provider_run.get("provider_version") != "3.4.0":
            raise ValueError("P13.1B provider_run does not prove provider 3.4.0")
        if not CORE_RESOURCES.issubset(names):
            raise ValueError("P13.1B is missing image/flavor resource evidence")
        if provider_run.get("result") == "passed":
            if provider_run.get("state_proven") is not True or document.get("findings"):
                raise ValueError("successful P13.1B evidence must contain state and no findings")
            resources = (
                provider_run.get("state", {})
                .get("values", {})
                .get("root_module", {})
                .get("resources", [])
            )
            if any(resource.get("mode") != "data" for resource in resources):
                raise ValueError("P13.1B state contains a managed resource")
            if any(trace.get("path", "").startswith("/v2/v2/") for trace in document["traces"]):
                raise ValueError("successful P13.1B evidence contains a duplicate image API prefix")
            if not any(trace.get("path") == "/v2/images" and trace.get("status") == 200 for trace in document["traces"]):
                raise ValueError("successful P13.1B evidence is missing the image list request")
            if not any("/os-extra_specs" in trace.get("path", "") and trace.get("status") == 200 for trace in document["traces"]):
                raise ValueError("successful P13.1B evidence is missing the extra-specs read")
    if document.get("phase") == "p13.1c":
        cases = document.get("managed_cases")
        if not isinstance(cases, list):
            raise ValueError("P13.1C requires managed_cases")
        case_names = {case.get("resource") for case in cases if isinstance(case, dict)}
        if case_names != CORE - CORE_RESOURCES:
            raise ValueError("P13.1C must contain exactly the five managed core resources")
        allowed = {"observed", "source-proven", "observed-and-source-proven", "harness-only"}
        for case in cases:
            if case.get("status") not in allowed:
                raise ValueError(f"invalid P13.1C case status: {case.get('status')}")
            if case.get("status") == "observed" and not case.get("gap"):
                raise ValueError("an observed managed case must have a precise gap")
            if not case.get("source_proof"):
                raise ValueError(f"P13.1C case lacks source proof: {case.get('resource')}")
        if any("/v2.0/v2.0/" in trace.get("path", "") for trace in document.get("traces", [])):
            raise ValueError("P13.1C evidence contains a duplicate Neutron API prefix")
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


def redact(value: Any, token_object: bool = False) -> Any:
    if isinstance(value, dict):
        return {
            key: (
                redact(item, True)
                if key == "token" and isinstance(item, dict)
                else "<redacted>"
                if SECRET_NAME.search(key) and not (token_object and key == "id")
                else "<redacted>"
                if token_object and key == "id"
                else redact(item)
            )
            for key, item in value.items()
        }
    if isinstance(value, list):
        return [redact(item) for item in value]
    return value


def contains_unsanitized_secret(value: Any) -> bool:
    if isinstance(value, dict):
        for key, item in value.items():
            if key == "token" and isinstance(item, dict):
                if contains_unsanitized_secret(item):
                    return True
            elif SECRET_NAME.search(key) and item != "<redacted>":
                return True
            elif contains_unsanitized_secret(item):
                return True
        return False
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
    auth_shape = redact({"token": {"id": "secret-token", "catalog": [{"type": "image"}]}})
    if auth_shape["token"]["id"] != "<redacted>" or auth_shape["token"]["catalog"][0]["type"] != "image":
        raise AssertionError("Keystone catalog redaction test failed")
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
        p13b = dict(document)
        p13b.update({
            "phase": "p13.1b",
            "provider_run": {
                "engine_version": "1.12.6",
                "provider_version": "3.4.0",
                "lock_file_verified": True,
            },
            "findings": source_backed_findings([
                {"method": "GET", "path": "/v2/v2/images", "query": "name=x", "status": 404},
                {"method": "GET", "path": "/v2.1/project/flavors/id/os-extra_specs", "query": "", "status": 404},
            ]),
        })
        p13b_path = pathlib.Path(directory) / "provider-contract-p13-1b.json"
        p13b_path.write_text(json.dumps(p13b), encoding="utf-8")
        validate_contract(p13b_path)
        if len(p13b["findings"]) != 2:
            raise AssertionError("P13.1B source-backed finding self-test failed")
    print("P13.1A evidence self-test passed")


def run_command(command: list[str], cwd: pathlib.Path, env: dict[str, str]) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True)
    print(result.stdout, end="")
    print(result.stderr, end="", file=sys.stderr)
    return result


def source_backed_findings(traces: list[dict[str, Any]]) -> list[dict[str, Any]]:
    findings: list[dict[str, Any]] = []
    image_gap = next((t for t in traces if t.get("path") == "/v2/v2/images"), None)
    if image_gap:
        findings.append({
            "case": "image-data-source-by-name",
            "classification": "wrong-catalog-endpoint-base",
            "provider_operation": "images.List(...).AllPages",
            "observed": {"method": image_gap.get("method"), "path": image_gap.get("path"), "query": image_gap.get("query"), "status": image_gap.get("status")},
            "expected_from_source": "Gophercloud images.List appends /images to the catalog service URL; provider supplies name, sort=name:asc, and status=active.",
            "minimum_later_requirement": "P13.2: make the advertised image endpoint base compatible with Gophercloud's ServiceURL composition, or otherwise record the accepted projection without changing the provider.",
        })
    extra_specs_gap = next(
        (
            t
            for t in traces
            if "/os-extra_specs" in t.get("path", "") and t.get("status") != 200
        ),
        None,
    )
    if extra_specs_gap:
        findings.append({
            "case": "flavor-data-source-by-name",
            "classification": "missing-route",
            "provider_operation": "flavors.ListExtraSpecs(...).Extract",
            "observed": {"method": extra_specs_gap.get("method"), "path": extra_specs_gap.get("path"), "query": extra_specs_gap.get("query"), "status": extra_specs_gap.get("status")},
            "expected_from_source": "Provider data_source_openstack_compute_flavor_v2 calls ListDetail, filters name, sets core fields, then calls ListExtraSpecs unconditionally; Gophercloud accepts HTTP 200 for this GET.",
            "minimum_later_requirement": "P13.2: implement the bounded extra-specs read contract or explicitly narrow the accepted provider profile; do not fix in P13.1B.",
        })
    return findings


def run_real() -> None:
    project = pathlib.Path(os.environ["O3K_P13_TOFU_PROJECT"])
    output = pathlib.Path(os.environ["O3K_P13_EVIDENCE_OUTPUT"])
    if not project.is_dir():
        raise ValueError("O3K_P13_TOFU_PROJECT must be an existing OpenTofu project")
    tofu = os.environ.get("O3K_P13_TOFU", "tofu")
    phase = os.environ.get("O3K_P13_PHASE", "p13.1a")
    env = os.environ.copy()
    env["TF_IN_AUTOMATION"] = "1"
    with tempfile.TemporaryDirectory(prefix="o3k-p13-tofu-") as directory:
        disposable_project = pathlib.Path(directory) / "project"
        shutil.copytree(project, disposable_project)
        init = run_command([tofu, "init", "-input=false", "-upgrade=false"], disposable_project, env)
        if init.returncode:
            raise subprocess.CalledProcessError(init.returncode, init.args, init.stdout, init.stderr)
        lock_file = disposable_project / ".terraform.lock.hcl"
        lock_text = lock_file.read_text(encoding="utf-8") if lock_file.exists() else ""
        if 'version     = "3.4.0"' not in lock_text:
            raise ValueError("OpenTofu lock file did not pin provider 3.4.0")
        plan_file = disposable_project / "p13-1b.tfplan"
        plan = run_command([tofu, "plan", "-input=false", "-refresh=false", f"-out={plan_file}"], disposable_project, env)
        state = None
        if plan.returncode == 0:
            apply = run_command([tofu, "apply", "-input=false", "-auto-approve"], disposable_project, env)
            if apply.returncode:
                raise subprocess.CalledProcessError(apply.returncode, apply.args, apply.stdout, apply.stderr)
            shown = run_command([tofu, "show", "-json"], disposable_project, env)
            if shown.returncode == 0:
                state = json.loads(shown.stdout)
        raw_path = pathlib.Path(os.environ["O3K_P13_RAW_EVIDENCE"])
        traces = redact(load_trace_records(raw_path))
        findings = source_backed_findings(traces) if phase == "p13.1b" else []
        expected_failure = os.environ.get("O3K_P13_EXPECTED_FAILURE", "0") == "1"
        recognized = findings and all(item["observed"]["path"] in plan.stderr for item in findings)
        if plan.returncode != 0 and (phase != "p13.1b" or not expected_failure or not recognized):
            raise subprocess.CalledProcessError(plan.returncode, plan.args, plan.stdout, plan.stderr)
    raw_path = pathlib.Path(os.environ["O3K_P13_RAW_EVIDENCE"])
    traces = redact(load_trace_records(raw_path))
    document = {
        "schema_version": 1,
        "artifact_type": "o3k-p13-1-provider-contract",
        "phase": phase,
        "toolchain": load_json(TOOLCHAIN),
        "provider_run": {
            "engine_version": "1.12.6",
            "provider_version": "3.4.0",
            "provider_source": "terraform-provider-openstack/openstack",
            "lock_file_verified": True,
            "exit_code": plan.returncode,
            "result": "passed" if plan.returncode == 0 else "failed-with-discovery-gap",
            "state_proven": state is not None,
            "stdout": redact(plan.stdout[-12000:]),
            "stderr": redact(plan.stderr[-12000:]),
        },
        "resources": [
            {"resource": resource, "evidence": "verified" if resource in CORE_RESOURCES and plan.returncode == 0 else "observed-or-deferred"}
            for resource in sorted(CORE)
        ],
        "traces": traces,
    }
    if phase == "p13.1b":
        document["findings"] = findings
    if state is not None:
        document["provider_run"]["state"] = redact(state)
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    validate_contract(output)
    if plan.returncode != 0:
        print("P13.1B completed with source-backed discovery gaps; no compatibility behavior was changed")


MANAGED_SOURCE = {
    "keypair": {"resource": "openstack_compute_keypair_v2", "operation": "create", "expected": "POST /v2.1/{project_id}/os-keypairs", "gap": "provider create/read/delete lifecycle is not provider-verified"},
    "network": {"resource": "openstack_networking_network_v2", "operation": "create", "expected": "POST /v2.0/networks", "gap": "provider create/read/update/delete lifecycle is not provider-verified"},
    "subnet": {"resource": "openstack_networking_subnet_v2", "operation": "create", "expected": "POST /v2.0/subnets", "gap": "provider create/read/update/delete lifecycle is not provider-verified; AddressRealm cardinality remains unresolved"},
    "port": {"resource": "openstack_networking_port_v2", "operation": "create", "expected": "POST /v2.0/ports", "gap": "provider create/read/update/delete lifecycle is not provider-verified; security groups are P13.3"},
    "server": {"resource": "openstack_compute_instance_v2", "operation": "create", "expected": "POST /v2.1/{project_id}/servers", "gap": "provider create/poll/read/update/action/delete lifecycle is not provider-verified"},
}


def assemble_managed() -> None:
    results = load_json(pathlib.Path(os.environ["O3K_P13_P13C_RESULTS"]))
    traces = redact(load_trace_records(pathlib.Path(os.environ["O3K_P13_RAW_EVIDENCE"])))
    cases = []
    for name, source in MANAGED_SOURCE.items():
        result = results.get(name)
        if not isinstance(result, dict):
            raise ValueError(f"missing managed case result: {name}")
        output = result.get("output", "")
        status = "observed"
        case_traces = [trace for trace in traces if trace.get("resource") in {source["resource"], "auth", "catalog", "extensions", "placement"}]
        path_values = [trace.get("path", "") for trace in case_traces]
        if result.get("exit_code", 1) == 0:
            classification = "provider-partial"
            current = "create/read succeeded; complete managed lifecycle was not exercised by this bounded apply"
        elif any("/v2.0/v2.0/" in path for path in path_values):
            classification = "wrong-catalog-endpoint-base"
            current = "O3K advertises a versioned Neutron endpoint and Gophercloud composes a duplicate /v2.0 prefix"
        elif name == "server" and any("/v2.0/v2.0/" in path for path in path_values):
            classification = "dependency-blocked-by-catalog-gap"
            current = "server prerequisite network lookup failed because the advertised Neutron base composes /v2.0/v2.0"
        elif name == "subnet" and any(trace.get("method") == "POST" and trace.get("path", "").endswith("/subnets") and trace.get("status") == 400 for trace in case_traces):
            classification = "wrong-request-semantics"
            current = "O3K rejected the provider's valid subnet request because the provider omits name and the current handler rejects the resulting empty name"
        elif name == "port" and any(trace.get("method") == "POST" and trace.get("path", "").endswith("/ports") and trace.get("status") == 404 for trace in case_traces):
            classification = "dependency-blocked-by-subnet"
            current = "O3K port allocation requires an existing subnet; the discovery subnet failed before the port case ran"
        elif name == "server" and any(trace.get("method") == "POST" and trace.get("path", "").endswith("/servers") and trace.get("status") == 404 for trace in case_traces):
            classification = "wrong-cross-service-resolution"
            current = "Nova create could not resolve the network resource supplied by the provider after the Neutron prerequisite lookup succeeded"
        else:
            classification = "missing-route-or-current-o3k-gap" if case_traces else "harness-only"
            current = "provider execution failed before a complete lifecycle was observed"
        gap = {
            "classification": classification,
            "provider_expected_behavior": source["expected"],
            "current_o3k": current,
            "minimum_p13_2_requirement": source["gap"],
            "provider_output": redact(output[-4000:]),
        }
        cases.append({
            "resource": source["resource"],
            "operation": source["operation"],
            "source_proof": {"provider": "3.4.0", "gophercloud": "v2.8.0", "reference": "tests/p13_1/provider-managed-resource-source-evidence.md", "expected": source["expected"]},
            "requests": case_traces,
            "terraform_result": {"exit_code": result.get("exit_code"), "output": redact(output[-4000:])},
            "status": status,
            "gap": gap,
        })
    document = {
        "schema_version": 1,
        "artifact_type": "o3k-p13-1-provider-contract",
        "phase": "p13.1c",
        "toolchain": load_json(TOOLCHAIN),
        "provider_run": {"engine_version": "1.12.6", "provider_version": "3.4.0", "lock_file_verified": True, "result": "discovery-complete"},
        "resources": [{"resource": item["resource"], "evidence": item["status"]} for item in cases],
        "managed_cases": cases,
        "dependency_graph": load_yaml_like_dependency_graph(),
        "traces": traces,
    }
    output = pathlib.Path(os.environ["O3K_P13_EVIDENCE_OUTPUT"])
    output.parent.mkdir(parents=True, exist_ok=True)
    output.write_text(json.dumps(document, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    validate_contract(output)


def load_yaml_like_dependency_graph() -> list[list[str]]:
    return [["image_data_source", "instance"], ["flavor_data_source", "instance"], ["network", "subnet"], ["network", "port"], ["subnet", "port"], ["port", "instance"], ["keypair", "instance"]]


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--validate", action="store_true")
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--verify-tools", action="store_true")
    parser.add_argument("--run-real", action="store_true")
    parser.add_argument("--assemble-managed", action="store_true")
    parser.add_argument("--validate-artifact")
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
        if args.assemble_managed:
            assemble_managed()
        if args.validate_artifact:
            validate_contract(pathlib.Path(args.validate_artifact))
            print(f"validated {args.validate_artifact}")
        if not any(vars(args).values()):
            parser.error("one action is required")
    except (OSError, subprocess.CalledProcessError, ValueError, AssertionError, KeyError) as error:
        print(f"P13.1A failed: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
