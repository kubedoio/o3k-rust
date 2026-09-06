#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=${1:-$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)}
CONTRACT="${ROOT_DIR}/contracts/openapi/native-iam.yaml"

python3 - "${ROOT_DIR}" "${CONTRACT}" <<'PY'
import pathlib
import re
import sys
import json

import jsonschema
import yaml

contract_path = pathlib.Path(sys.argv[2]).resolve()
data = yaml.safe_load(contract_path.read_text(encoding="utf-8"))

assert data["openapi"] == "3.1.2"
assert data["x-o3k-openapi-policy"] == {"target": "3.1.2", "status": "target"}
assert data["x-o3k-baseline"]["coverage"] == "native-iam"

expected = {
    ("/identity/tokens", "post", "issueNativeToken"),
    ("/identity/me", "get", "getNativeIdentityContext"),
    ("/operator/profile", "get", "getOperatorProfile"),
}
actual = {
    (path, method, operation["operationId"])
    for path, item in data["paths"].items()
    for method, operation in item.items()
    if method in {"get", "post", "put", "patch", "delete"}
}
assert actual == expected, f"native IAM operation drift: expected {expected}, got {actual}"

for path, method, operation_id in expected:
    operation = data["paths"][path][method]
    assert operation["responses"], f"{operation_id} has no responses"
    if path != "/identity/tokens":
        assert operation["security"] == [{"bearerAuth": []}], f"{operation_id} security drift"

assert "application/problem+json" in json.dumps(data["components"]["responses"])
codes = set(data["components"]["schemas"]["ProblemDetails"]["properties"]["code"]["enum"])
assert codes == {
    "BAD_REQUEST", "UNSUPPORTED_OPERATION", "UNAUTHORIZED", "FORBIDDEN",
    "RESOURCE_NOT_FOUND", "CONFLICT", "REQUEST_TOO_LARGE", "INVALID_CURSOR",
    "UNSUPPORTED_MEDIA_TYPE", "NOT_AVAILABLE", "INTERNAL_ERROR",
}

resolver = jsonschema.RefResolver.from_schema(data)
request_schema = data["components"]["schemas"]["NativeTokenRequest"]
for name in ("ProjectFederatedRequest", "SystemOperatorRequest"):
    example = data["components"]["examples"][name]["value"]
    jsonschema.Draft202012Validator(request_schema, resolver=resolver).validate(example)

text = contract_path.read_text(encoding="utf-8")
assert "REDACTED_EXTERNAL_TOKEN" in text
for forbidden in ("client_secret", "private_key", "jwks_cache", "Authorization: Bearer ey"):
    assert forbidden not in text, f"secret/private trust material leaked: {forbidden}"
assert not re.search(r"eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}", text)

print("native IAM OpenAPI contract validated: operations, refs, examples, security, errors, and redaction")
PY

generated=$(mktemp)
trap 'rm -f "${generated}"' EXIT
npx --yes openapi-typescript@7.8.0 "${CONTRACT}" -o "${generated}" >/dev/null
grep -q 'issueNativeToken' "${generated}"
grep -q 'getNativeIdentityContext' "${generated}"
grep -q 'getOperatorProfile' "${generated}"
echo "native IAM generated-client smoke passed"
