#!/usr/bin/env bash
set -euo pipefail

# This gate deliberately validates contract structure and repository linkage.
# It does not claim complete OpenAPI semantic, example, or runtime validation.
ROOT_DIR=${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}

python3 - "${ROOT_DIR}" <<'PY'
import json
import pathlib
import re
import sys

try:
    import yaml
except ImportError as exc:
    raise SystemExit("OpenAPI governance requires the dependency-light python3-yaml package") from exc


root = pathlib.Path(sys.argv[1]).resolve()
contract_dir = root / "contracts" / "openapi"
baseline_path = root / "docs" / "specs" / "testlab-api-baseline.json"

if not contract_dir.is_dir():
    raise AssertionError(f"missing OpenAPI contract directory: {contract_dir}")
documents = sorted(contract_dir.glob("*.yaml"))
if not documents:
    raise AssertionError(f"no OpenAPI YAML documents found in {contract_dir}")


class UniqueKeyLoader(yaml.SafeLoader):
    """Safe YAML loader that rejects duplicate mapping keys."""


def construct_unique_mapping(loader, node, deep=False):
    mapping = {}
    for key_node, value_node in node.value:
        key = loader.construct_object(key_node, deep=deep)
        if key in mapping:
            raise yaml.constructor.ConstructorError(
                "while constructing a mapping",
                node.start_mark,
                f"found duplicate key {key!r}",
                key_node.start_mark,
            )
        mapping[key] = loader.construct_object(value_node, deep=deep)
    return mapping


UniqueKeyLoader.add_constructor(
    yaml.resolver.BaseResolver.DEFAULT_MAPPING_TAG, construct_unique_mapping
)


METHODS = {"get", "put", "post", "delete", "options", "head", "patch", "trace"}
PATH_ITEM_KEYS = METHODS | {"summary", "description", "parameters", "servers", "$ref"}
VERSION_RE = re.compile(r"^3\.1\.(?:0|2)$")
IDENTIFIER_RE = re.compile(r"^[A-Za-z][A-Za-z0-9._-]*$")
PATH_PARAMETER_RE = re.compile(r"\{([^{}]+)\}")
RESPONSE_KEY_RE = re.compile(r"^(?:default|[1-5](?:\d{2}|XX))$")


def fail(document, message):
    raise AssertionError(f"{document.relative_to(root)}: {message}")


def resolve_local_ref(document, value, target):
    if not isinstance(value, str) or not value.startswith("#"):
        return
    if not value.startswith("#/"):
        fail(document, f"local $ref must use a JSON Pointer: {value!r}")
    current = target
    for raw_part in value[2:].split("/"):
        part = raw_part.replace("~1", "/").replace("~0", "~")
        if not isinstance(current, dict) or part not in current:
            fail(document, f"unresolved local $ref {value!r}")
        current = current[part]


def validate_refs(document, value, target):
    if isinstance(value, dict):
        if "$ref" in value:
            resolve_local_ref(document, value["$ref"], target)
        for child in value.values():
            validate_refs(document, child, target)
    elif isinstance(value, list):
        for child in value:
            validate_refs(document, child, target)


def validate_parameter(document, parameter, location):
    if not isinstance(parameter, dict):
        fail(document, f"{location} parameter must be a mapping")
    if "$ref" in parameter:
        return
    if not isinstance(parameter.get("name"), str) or not parameter["name"]:
        fail(document, f"{location} parameter has no name")
    if parameter.get("in") not in {"path", "query", "header", "cookie"}:
        fail(document, f"{location} parameter has invalid 'in' value")
    if parameter.get("in") == "path" and parameter.get("required") is not True:
        fail(document, f"{location} path parameter is not required")


def validate_responses(document, operation, location):
    responses = operation.get("responses")
    if not isinstance(responses, dict) or not responses:
        fail(document, f"{location} must declare at least one response")
    for status, response in responses.items():
        if not isinstance(status, str) or not RESPONSE_KEY_RE.fullmatch(status):
            fail(document, f"{location} has invalid response key {status!r}")
        if not isinstance(response, dict) and not (isinstance(response, str) and response.startswith("#")):
            fail(document, f"{location} response {status!r} must be a mapping or local $ref")


def validate_document(document):
    try:
        with document.open(encoding="utf-8") as stream:
            loader = UniqueKeyLoader(stream)
            try:
                data = loader.get_single_data()
            finally:
                loader.dispose()
    except yaml.YAMLError as exc:
        fail(document, f"invalid YAML: {exc}")
    if not isinstance(data, dict):
        fail(document, "document root must be a mapping")

    version = data.get("openapi")
    if not isinstance(version, str) or not VERSION_RE.fullmatch(version):
        fail(document, "openapi must be 3.1.0 (transitional) or 3.1.2 (target)")
    policy = data.get("x-o3k-openapi-policy")
    if not isinstance(policy, dict) or policy.get("target") != "3.1.2":
        fail(document, "x-o3k-openapi-policy.target must declare 3.1.2")
    expected_status = "transitional" if version == "3.1.0" else "target"
    if policy.get("status") != expected_status:
        fail(document, f"version {version} requires policy status {expected_status!r}")

    baseline = data.get("x-o3k-baseline")
    if not isinstance(baseline, dict):
        fail(document, "x-o3k-baseline metadata is required")
    if baseline.get("path") != "docs/specs/testlab-api-baseline.json":
        fail(document, "x-o3k-baseline.path must link the normative TestLab baseline")
    if baseline.get("status") not in {"partial", "complete"}:
        fail(document, "x-o3k-baseline.status must be partial or complete")
    if baseline.get("coverage") != "bootstrap":
        fail(document, "bootstrap contract must declare bootstrap baseline coverage")

    paths = data.get("paths")
    if not isinstance(paths, dict) or not paths:
        fail(document, "paths must be a non-empty mapping")
    operation_ids = set()
    for path, path_item in paths.items():
        if not isinstance(path, str) or not path.startswith("/") or "//" in path:
            fail(document, f"invalid path key {path!r}")
        template_names = PATH_PARAMETER_RE.findall(path)
        if "{" in path or "}" in path:
            if path.count("{") != path.count("}") or len(template_names) != path.count("{"):
                fail(document, f"malformed path template {path!r}")
        if not isinstance(path_item, dict):
            fail(document, f"path item {path!r} must be a mapping")
        unknown_keys = set(path_item) - PATH_ITEM_KEYS
        if unknown_keys:
            fail(document, f"path {path!r} has invalid keys {sorted(unknown_keys)!r}")
        path_parameters = path_item.get("parameters", [])
        if not isinstance(path_parameters, list):
            fail(document, f"path {path!r} parameters must be a list")
        for index, parameter in enumerate(path_parameters):
            validate_parameter(document, parameter, f"{path} parameter {index}")
        declared_path_parameters = {
            (p.get("name"), p.get("in"))
            for p in path_parameters
            if isinstance(p, dict) and "$ref" not in p
        }
        for method, operation in path_item.items():
            if method in {"summary", "description", "parameters", "servers", "$ref"}:
                continue
            if method not in METHODS:
                fail(document, f"path {path!r} has invalid method key {method!r}")
            if not isinstance(operation, dict):
                fail(document, f"{method.upper()} {path} must be a mapping")
            operation_id = operation.get("operationId")
            if not isinstance(operation_id, str) or not IDENTIFIER_RE.fullmatch(operation_id):
                fail(document, f"{method.upper()} {path} has invalid operationId")
            if operation_id in operation_ids:
                fail(document, f"duplicate operationId {operation_id!r}")
            operation_ids.add(operation_id)
            operation_parameters = operation.get("parameters", [])
            if not isinstance(operation_parameters, list):
                fail(document, f"{method.upper()} {path} parameters must be a list")
            for index, parameter in enumerate(operation_parameters):
                validate_parameter(document, parameter, f"{method.upper()} {path} parameter {index}")
            declared = declared_path_parameters | {
                (p.get("name"), p.get("in"))
                for p in operation_parameters
                if isinstance(p, dict) and "$ref" not in p
            }
            missing = [name for name in template_names if (name, "path") not in declared]
            if missing:
                fail(document, f"{method.upper()} {path} lacks required path parameter(s): {missing}")
            validate_responses(document, operation, f"{method.upper()} {path}")

    validate_refs(document, data, data)
    return len(operation_ids), version, baseline["status"]


if not baseline_path.is_file():
    raise AssertionError(f"missing linked baseline: {baseline_path}")
try:
    baseline = json.loads(baseline_path.read_text(encoding="utf-8"))
except json.JSONDecodeError as exc:
    raise AssertionError(f"linked baseline is not valid JSON: {exc}") from exc
if baseline.get("status") != "normative" or not isinstance(baseline.get("operations"), list):
    raise AssertionError("linked baseline must be normative and contain operations")

results = [validate_document(document) for document in documents]
print(
    "validated OpenAPI structural governance only: "
    f"{len(documents)} document(s), {sum(item[0] for item in results)} operation(s), "
    "local refs resolved, baseline linkage present; semantic/example validation not claimed"
)
PY

if [[ "${OPENAPI_GOVERNANCE_SKIP_MUTATIONS:-0}" == 1 ]]; then
    exit 0
fi

fixture_root=$(mktemp -d)
trap 'rm -rf "${fixture_root}"' EXIT
mkdir -p "${fixture_root}/contracts/openapi" "${fixture_root}/docs/specs"
cp "${ROOT_DIR}/contracts/openapi"/*.yaml "${fixture_root}/contracts/openapi/"
cp "${ROOT_DIR}/docs/specs/testlab-api-baseline.json" "${fixture_root}/docs/specs/"

expect_rejection() {
    local label=$1
    local expected=$2
    shift 2
    local output
    if output=$(OPENAPI_GOVERNANCE_SKIP_MUTATIONS=1 bash "$0" "${fixture_root}" 2>&1); then
        printf 'expected OpenAPI governance fixture to fail: %s\n' "${label}" >&2
        exit 1
    fi
    if [[ "${output}" != *"${expected}"* ]]; then
        printf 'fixture %s failed without expected diagnostic %q:\n%s\n' \
            "${label}" "${expected}" "${output}" >&2
        exit 1
    fi
}

mutate_fixture() {
    local expression=$1
    python3 - "${fixture_root}/contracts/openapi/bootstrap.yaml" "${expression}" <<'PY'
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
expression = sys.argv[2]
text = path.read_text(encoding="utf-8")
old, new = expression.split("|", 1)
if old not in text:
    raise SystemExit(f"fixture source text not found: {old!r}")
path.write_text(text.replace(old, new, 1), encoding="utf-8")
PY
}

mutate_fixture 'operationId: getHealth|operationId: issuePasswordScopedToken'
expect_rejection "duplicate operationId" "duplicate operationId"

cp "${ROOT_DIR}/contracts/openapi/bootstrap.yaml" "${fixture_root}/contracts/openapi/bootstrap.yaml"
mutate_fixture 'openapi: 3.1.0|openapi: 3.0.3'
expect_rejection "unsupported OpenAPI version" "openapi must be 3.1.0"

cp "${ROOT_DIR}/contracts/openapi/bootstrap.yaml" "${fixture_root}/contracts/openapi/bootstrap.yaml"
mutate_fixture "\$ref: '#/components/schemas/Status'|\$ref: '#/components/schemas/Missing'"
expect_rejection "unresolved local reference" 'unresolved local $ref'

echo "OpenAPI governance negative fixtures passed"
