#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
POLICY="${ROOT_DIR}/docs/supply-chain/advisory-exceptions.yaml"
DENY="${ROOT_DIR}/deny.toml"
WORKFLOW="${ROOT_DIR}/.github/workflows/ci.yml"

python3 - "${POLICY}" "${DENY}" "${WORKFLOW}" <<'PY'
import pathlib
import re
import sys

try:
    import yaml
except ImportError as error:
    raise SystemExit("tests/supply-chain-governance.sh requires Python PyYAML") from error


class UniqueKeyLoader(yaml.SafeLoader):
    """Safe YAML loader that rejects duplicate mapping keys."""


def construct_unique_mapping(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise ValueError(f"duplicate YAML key: {key!r}")
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct_unique_mapping
)


def fail(message):
    raise AssertionError(message)


def mapping(value, path, keys):
    if type(value) is not dict:
        fail(f"{path}: expected mapping")
    actual = set(value)
    expected = set(keys)
    if actual != expected:
        fail(f"{path}: expected keys {sorted(expected)}, got {sorted(actual)}")
    return value


def string(value, path):
    if type(value) is not str or not value:
        fail(f"{path}: expected non-empty string")


policy_path, deny_path, workflow_path = map(pathlib.Path, sys.argv[1:])
try:
    policy = yaml.load(policy_path.read_text(encoding="utf-8"), Loader=UniqueKeyLoader)
except (OSError, ValueError, yaml.YAMLError) as error:
    raise SystemExit(f"cannot parse {policy_path}: {error}") from error

policy = mapping(policy, "policy", ("schema_version", "advisory_database", "exceptions"))
if type(policy["schema_version"]) is not int or policy["schema_version"] != 1:
    fail("policy.schema_version must be 1")

database = mapping(
    policy["advisory_database"],
    "policy.advisory_database",
    ("repository", "commit", "fetch_mode"),
)
if database["repository"] != "https://github.com/RustSec/advisory-db.git":
    fail("policy.advisory_database.repository must be the RustSec advisory database")
if not re.fullmatch(r"[0-9a-f]{40}", database["commit"]):
    fail("policy.advisory_database.commit must be a 40-character lowercase commit")
if database["fetch_mode"] != "pinned":
    fail("policy.advisory_database.fetch_mode must be pinned")

exceptions = policy["exceptions"]
if type(exceptions) is not list or len(exceptions) != 1:
    fail("policy.exceptions must contain exactly one approved exception")
exception = mapping(
    exceptions[0],
    "policy.exceptions[0]",
    ("id", "package", "affected_version", "owner", "rationale", "review_trigger"),
)
expected = {
    "id": "RUSTSEC-2023-0071",
    "package": "rsa",
    "affected_version": "0.9.10",
    "owner": "O3K maintainers",
    "rationale": (
        "Retained only in the lockfile through SQLx's optional MySQL support; "
        "this workspace enables SQLite only and cargo tree has no active RSA path."
    ),
    "review_trigger": "Reconsider if another SQLx database feature is enabled.",
}
for key, value in expected.items():
    string(exception[key], f"policy.exceptions[0].{key}")
    if exception[key] != value:
        fail(f"policy.exceptions[0].{key} does not match the documented exception")

deny = deny_path.read_text(encoding="utf-8")
if not re.search(r"(?m)^\s*ignore\s*=\s*\[\s*\]\s*$", deny):
    fail("deny.toml must not carry unreviewed advisory ignores")

workflow = workflow_path.read_text(encoding="utf-8")
if workflow.count(database["commit"]) != 1:
    fail("CI must contain exactly one occurrence of the pinned advisory DB commit")
if workflow.count("--ignore RUSTSEC-2023-0071") != 1:
    fail("CI must ignore exactly the machine-readable exception ID")
if workflow.count("cargo audit") != 1:
    fail("CI must contain exactly one cargo audit invocation")
audit_line = next(line for line in workflow.splitlines() if "cargo audit" in line)
for required in ('--db "${advisory_db}"', "--no-fetch", "--stale", "--ignore RUSTSEC-2023-0071"):
    if required not in audit_line:
        fail(f"cargo audit invocation is missing {required}")

print("supply-chain advisory governance passed")
PY
