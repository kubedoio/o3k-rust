#!/usr/bin/env python3
"""Shared validator for the release E2E evidence resource contract.

Validates the `resources` and `cleanup.resources` objects of an
`openstack-cli-e2e` release evidence artifact against the normative
machine-readable contract in contracts/release-e2e-evidence.schema.json.
That schema is the single source of truth for the exact resource key sets:
packaging/release-gate.sh, tests/openstack-cli-libvirt.sh, and the test
suite all consume this validator, so a key-set change on one side without
the other fails. Only the schema subset used here (required, properties,
additionalProperties, type, minLength, enum) is implemented; no third-party
imports.

Usage:
  validate-release-e2e-evidence.py [--schema PATH] ARTIFACT.json
  validate-release-e2e-evidence.py --example

--example prints a valid artifact to stdout built only from the schema's
required key sets, so tests never hand-duplicate the key lists.
"""

import argparse
import json
import os
import sys
import time

DEFAULT_SCHEMA = os.path.join(
    os.path.dirname(os.path.abspath(__file__)),
    "..",
    "contracts",
    "release-e2e-evidence.schema.json",
)


def load_contract(schema_path):
    """Read the schema and return (schema, resource_keys, cleanup_keys)."""
    try:
        with open(schema_path, encoding="utf-8") as stream:
            schema = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        raise SystemExit(f"cannot read contract schema {schema_path}: {error}")
    try:
        resources_spec = schema["properties"]["resources"]
        cleanup_spec = schema["properties"]["cleanup"]["properties"]["resources"]
        resource_keys = list(resources_spec["required"])
        cleanup_keys = list(cleanup_spec["required"])
    except (KeyError, TypeError):
        raise SystemExit(
            f"contract schema {schema_path} is not in the expected shape"
        )
    if set(resource_keys) != set(resources_spec.get("properties", {})):
        raise SystemExit(
            f"contract schema {schema_path}: resources required/properties mismatch"
        )
    if set(cleanup_keys) != set(cleanup_spec.get("properties", {})):
        raise SystemExit(
            f"contract schema {schema_path}: cleanup.resources required/properties mismatch"
        )
    return schema, resource_keys, cleanup_keys


def check_object(value, required_keys, spec, label):
    """One error string per violation of the exact-required-set rule."""
    if not isinstance(value, dict):
        return [f"{label}: must be an object"]
    errors = []
    missing = set(required_keys) - set(value)
    unexpected = set(value) - set(required_keys)
    if missing:
        errors.append(f"{label}: missing required keys: {', '.join(sorted(missing))}")
    if unexpected:
        errors.append(f"{label}: unexpected keys: {', '.join(sorted(unexpected))}")
    for key in required_keys:
        if key not in value:
            continue
        prop_spec = spec["properties"][key]
        if "enum" in prop_spec:
            if value[key] not in prop_spec["enum"]:
                errors.append(
                    f"{label}.{key}: must be one of: {', '.join(prop_spec['enum'])}"
                )
        else:
            min_length = prop_spec.get("minLength", 1)
            if not isinstance(value[key], str) or len(value[key]) < min_length:
                errors.append(f"{label}.{key}: must be a non-empty string")
    return errors


def check_artifact(artifact, schema, resource_keys, cleanup_keys):
    errors = []
    if not isinstance(artifact, dict):
        return ["artifact root must be an object"]
    resources = artifact.get("resources")
    if resources is not None:
        errors.extend(
            check_object(
                resources, resource_keys, schema["properties"]["resources"], "resources"
            )
        )
    cleanup = artifact.get("cleanup")
    if cleanup is not None:
        if not isinstance(cleanup, dict):
            errors.append("cleanup: must be an object")
        else:
            cleanup_resources = cleanup.get("resources")
            if cleanup_resources is not None:
                errors.extend(
                    check_object(
                        cleanup_resources,
                        cleanup_keys,
                        schema["properties"]["cleanup"]["properties"]["resources"],
                        "cleanup.resources",
                    )
                )
    return errors


def example_artifact(resource_keys, cleanup_keys):
    """A valid artifact built only from the schema's required key sets."""
    resources = {
        key: "00000000-0000-0000-0000-0000000000%02d" % (index + 1)
        for index, key in enumerate(resource_keys)
    }
    # acceptance evidence is required by packaging/release-gate.sh but is not
    # part of the schema contract; it keeps --example usable as a gate fixture.
    return {
        "artifact_type": "openstack-cli-e2e",
        "profile": "libvirt",
        "redacted": True,
        "status": "passed",
        "resources": resources,
        "cleanup": {
            "status": "passed",
            "resources": {key: "verified_absent" for key in cleanup_keys},
        },
        "finished_at": int(time.time()),
        "public_api_only": True,
        "lifecycle": {
            name: True
            for name in ("create", "show", "list", "stop", "start", "reboot", "console", "delete")
        },
        "acceptance": {
            "status": "ACTIVE",
            "fixed_ip": "192.0.2.2",
            "config_drive": True,
            "console_boot_marker": True,
            "restart": {"status": "ACTIVE", "fixed_ip": "192.0.2.2", "config_drive": True},
        },
    }


def main(argv):
    parser = argparse.ArgumentParser(
        description=(
            "Validate release E2E evidence resource membership against "
            "contracts/release-e2e-evidence.schema.json"
        )
    )
    parser.add_argument(
        "--schema",
        default=DEFAULT_SCHEMA,
        help="path to the contract schema (default: alongside this script)",
    )
    parser.add_argument(
        "--example",
        action="store_true",
        help="print a valid example artifact derived from the schema to stdout",
    )
    parser.add_argument("artifact", nargs="?", help="release E2E evidence artifact JSON")
    args = parser.parse_args(argv)
    schema, resource_keys, cleanup_keys = load_contract(args.schema)
    if args.example:
        json.dump(
            example_artifact(resource_keys, cleanup_keys),
            sys.stdout,
            indent=2,
            sort_keys=True,
        )
        sys.stdout.write("\n")
        return 0
    if not args.artifact:
        parser.error("an artifact path is required (or pass --example)")
    try:
        with open(args.artifact, encoding="utf-8") as stream:
            artifact = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        print(f"cannot read artifact {args.artifact}: {error}", file=sys.stderr)
        return 1
    errors = check_artifact(artifact, schema, resource_keys, cleanup_keys)
    for error in errors:
        print(f"{args.artifact}: {error}", file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
