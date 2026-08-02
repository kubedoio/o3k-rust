#!/usr/bin/env bash
set -euo pipefail

python3 - <<'PY'
import json
import re
import subprocess
from pathlib import Path

try:
    import yaml
except ImportError as error:
    raise SystemExit(
        "tests/compatibility-target.sh requires Python PyYAML to parse "
        "compatibility/openstack-targets.yaml"
    ) from error


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


root = Path.cwd()


def fail(path, message):
    raise AssertionError(f"{path}: {message}")


def mapping(value, path, keys):
    if type(value) is not dict:
        fail(path, f"expected mapping, got {type(value).__name__}")
    actual = set(value)
    required = set(keys)
    missing = required - actual
    unknown = actual - required
    if missing:
        fail(path, f"missing required keys: {sorted(missing)}")
    if unknown:
        fail(path, f"unknown keys: {sorted(unknown)}")
    return value


def exact(value, expected, path):
    if type(value) is not expected:
        fail(path, f"expected {expected.__name__}, got {type(value).__name__}")


def string(value, path):
    exact(value, str, path)


def string_list(value, path):
    exact(value, list, path)
    for index, item in enumerate(value):
        string(item, f"{path}[{index}]")
    if len(value) != len(set(value)):
        fail(path, "values must be unique")


def header(value, path, service):
    if service not in {"compute", "placement"}:
        if value is not None:
            fail(path, "must be null for a service without microversions")
        return
    fields = mapping(value, path, ("name", "value_template"))
    string(fields["name"], f"{path}.name")
    string(fields["value_template"], f"{path}.value_template")
    expected = f"{service} {{microversion}}"
    if fields != {"name": "OpenStack-API-Version", "value_template": expected}:
        fail(path, f"must use the exact service-qualified template {expected!r}")


def version(value, path):
    if value is None:
        return None
    string(value, path)
    if not re.fullmatch(r"\d+\.\d+", value):
        fail(path, "must be a dotted numeric microversion")
    return tuple(int(part) for part in value.split("."))


def window(value, path):
    if value is None:
        return None
    string(value, path)
    match = re.fullmatch(r"(\d+\.\d+)-(\d+\.\d+)", value)
    if not match:
        fail(path, "must be a dotted numeric range such as 2.1-2.1")
    low = tuple(int(part) for part in match.group(1).split("."))
    high = tuple(int(part) for part in match.group(2).split("."))
    if low > high:
        fail(path, "lower bound must not exceed upper bound")
    return low, high


service_keys = (
    "name",
    "api_major",
    "documentation_base",
    "min_advertised_microversion",
    "max_advertised_microversion",
    "implemented_microversion_window",
    "default_microversion",
    "request_version_header",
    "response_version_header",
    "supported_extensions",
    "unsupported_operations",
    "portable_contract_status",
    "protected_runner_status",
)
profile_keys = ("id", "purpose", "release_series", "codename", "release_source", "services")
known_releases = {"2026.1": "Gazpacho", "2025.2": "Flamingo"}
required_services = {"identity", "image", "network", "compute", "placement"}
status_values = {"missing", "partial", "planned", "implemented", "portable-contract-verified"}
protected_values = {"not-verified", "portable-only", "protected-runner-verified"}

manifest_path = root / "compatibility/openstack-targets.yaml"
try:
    with manifest_path.open() as stream:
        manifest = yaml.load(stream, Loader=UniqueKeyLoader)
except (OSError, ValueError, yaml.YAMLError) as error:
    raise SystemExit(f"cannot parse {manifest_path}: {error}") from error

manifest = mapping(
    manifest,
    "manifest",
    ("schema_version", "decision", "forbidden_release_pairings", "client_versions", "profiles"),
)
exact(manifest["schema_version"], int, "manifest.schema_version")
if manifest["schema_version"] != 1:
    fail("manifest.schema_version", "must be 1")
string(manifest["decision"], "manifest.decision")
if manifest["decision"] != "static":
    fail("manifest.decision", "must be static")

pairings = manifest["forbidden_release_pairings"]
exact(pairings, list, "manifest.forbidden_release_pairings")
for index, pairing in enumerate(pairings):
    pairing = mapping(pairing, f"manifest.forbidden_release_pairings[{index}]", ("release_series", "codename"))
    string(pairing["release_series"], f"manifest.forbidden_release_pairings[{index}].release_series")
    string(pairing["codename"], f"manifest.forbidden_release_pairings[{index}].codename")
forbidden_pairs = {
    (pairing["release_series"], pairing["codename"])
    for pairing in manifest["forbidden_release_pairings"]
}

clients = mapping(manifest["client_versions"], "manifest.client_versions", ("python-openstackclient", "openstacksdk"))
for key, value in clients.items():
    string(value, f"manifest.client_versions.{key}")

profiles = manifest["profiles"]
exact(profiles, list, "manifest.profiles")
if not profiles:
    fail("manifest.profiles", "must not be empty")
profile_ids = []
for index, profile in enumerate(profiles):
    path = f"manifest.profiles[{index}]"
    profile = mapping(profile, path, profile_keys)
    for key in ("id", "purpose", "release_series", "codename", "release_source"):
        string(profile[key], f"{path}.{key}")
    if profile["purpose"] not in {"primary", "backward-compatibility"}:
        fail(f"{path}.purpose", "unknown profile purpose")
    pair = (profile["release_series"], profile["codename"])
    if known_releases.get(profile["release_series"]) != profile["codename"]:
        fail(path, f"invalid release series/codename pairing: {pair!r}")
    if pair in forbidden_pairs:
        fail(path, f"uses a forbidden release pairing: {pair!r}")
    expected_id = f"{profile['codename'].lower()}-{profile['release_series']}"
    if profile["id"] != expected_id:
        fail(f"{path}.id", f"must be {expected_id!r}")
    if not re.fullmatch(r"https://releases\.openstack\.org/[a-z0-9-]+/index\.html", profile["release_source"]):
        fail(f"{path}.release_source", "must be an official release URL")
    profile_ids.append(profile["id"])
    services = profile["services"]
    exact(services, list, f"{path}.services")
    service_names = []
    for service_index, service in enumerate(services):
        service_path = f"{path}.services[{service_index}]"
        service = mapping(service, service_path, service_keys)
        for key in ("name", "api_major", "documentation_base"):
            string(service[key], f"{service_path}.{key}")
        if not re.fullmatch(r"https://docs\.openstack\.org/.+", service["documentation_base"]):
            fail(f"{service_path}.documentation_base", "must be an official documentation URL")
        service_name = service["name"]
        if service_name not in required_services:
            fail(f"{service_path}.name", "unknown service")
        expected_api_major = {
            "identity": "keystone-v3",
            "image": "glance-v2",
            "network": "neutron-v2",
            "compute": "nova-v2.1",
            "placement": "placement-v1",
        }[service_name]
        if service["api_major"] != expected_api_major:
            fail(f"{service_path}.api_major", f"must be {expected_api_major!r}")
        service_names.append(service_name)
        min_version = version(service["min_advertised_microversion"], f"{service_path}.min_advertised_microversion")
        max_version = version(service["max_advertised_microversion"], f"{service_path}.max_advertised_microversion")
        implemented = window(service["implemented_microversion_window"], f"{service_path}.implemented_microversion_window")
        default = version(service["default_microversion"], f"{service_path}.default_microversion")
        if service_name == "compute":
            if min_version is None or max_version is None or implemented is None or default is None:
                fail(service_path, "compute requires all microversion fields")
            if not (min_version <= max_version and min_version <= implemented[0] <= implemented[1] <= max_version):
                fail(service_path, "implemented window must be inside advertised bounds")
        elif service_name == "placement":
            if min_version is not None or max_version is not None or implemented is None or default is None:
                fail(service_path, "placement requires null advertised bounds and an implemented window")
        elif any(value is not None for value in (min_version, max_version, implemented, default)):
            fail(service_path, "identity, image, and network must not declare microversions")
        header(service["request_version_header"], f"{service_path}.request_version_header", service_name)
        header(service["response_version_header"], f"{service_path}.response_version_header", service_name)
        string_list(service["supported_extensions"], f"{service_path}.supported_extensions")
        string_list(service["unsupported_operations"], f"{service_path}.unsupported_operations")
        string(service["portable_contract_status"], f"{service_path}.portable_contract_status")
        if service["portable_contract_status"] not in status_values:
            fail(f"{service_path}.portable_contract_status", "unknown evidence state")
        string(service["protected_runner_status"], f"{service_path}.protected_runner_status")
        if service["protected_runner_status"] not in protected_values:
            fail(f"{service_path}.protected_runner_status", "unknown evidence state")
    if len(service_names) != len(set(service_names)):
        fail(f"{path}.services", "service IDs must be unique")
    if set(service_names) != required_services:
        fail(f"{path}.services", f"must contain exactly {sorted(required_services)}")

if len(profile_ids) != len(set(profile_ids)):
    fail("manifest.profiles", "profile IDs must be unique")
if set(profile_ids) != {"gazpacho-2026.1", "flamingo-2025.2"}:
    fail("manifest.profiles", "must contain the Gazpacho primary and Flamingo backward profiles")
if sorted(profile["purpose"] for profile in profiles) != ["backward-compatibility", "primary"]:
    fail("manifest.profiles", "must contain exactly one primary and one backward-compatibility profile")

target = json.loads((root / "docs/compatibility/target.json").read_text())
baseline = json.loads((root / "docs/specs/testlab-api-baseline.json").read_text())
ci = (root / ".github/workflows/ci.yml").read_text()

assert target["decision"] == "static"
assert target["rust"]["toolchain"] == "1.97.1"
assert target["rust"]["cargo_rust_version"] == "1.97.1"
openstack = target["openstack"]
assert openstack["primary_profile"] == "gazpacho-2026.1"
assert openstack["backward_compatibility_profiles"] == ["flamingo-2025.2"]
assert openstack["forbidden_release_pairings"] == [
    {"release_series": "2026.1", "codename": "Flamingo"}
]
assert baseline["openstack_compatibility"] == {
    "series": "2026.1",
    "codename": "Gazpacho",
    "selection": "static",
    "specification": "compatibility/openstack-targets.yaml",
    "backward_compatibility": {"series": "2025.2", "codename": "Flamingo"},
}
assert "reads rust-toolchain.toml" in ci
assert not re.search(r"^\s+toolchain:\s*", ci, re.MULTILINE)
for path in (root / "Cargo.toml", root / "rust-toolchain.toml", root / ".github/workflows/ci.yml"):
    assert "1.85" not in path.read_text(), path
assert 'rust-version = "1.97.1"' in (root / "Cargo.toml").read_text()
assert 'channel = "1.97.1"' in (root / "rust-toolchain.toml").read_text()
version_output = subprocess.run(
    ["rustc", "--version"], check=True, capture_output=True, text=True
).stdout.strip()
assert version_output.startswith("rustc 1.97.1 "), version_output
adr = (root / "docs/adr/ADR-0153-static-rust-and-openstack-release-policy.md").read_text()
spec = (root / "docs/specs/SPEC-0016-static-compatibility-target.md").read_text()
assert "#332" in adr and "2026.1 Gazpacho" in adr and "2025.2 Flamingo" in adr
assert "must not select a different profile" in adr
assert "2026.1" in spec and "Gazpacho" in spec and "Flamingo" in spec
assert "schema-validated" in spec and "value_template" in spec
for source in target["sources"]:
    assert re.match(r"https://(docs\.openstack\.org|releases\.openstack\.org|blog\.rust-lang\.org)/", source), source
print("validated static Rust 1.97.1, schema-checked OpenStack profiles, unique services, and version templates")
PY
