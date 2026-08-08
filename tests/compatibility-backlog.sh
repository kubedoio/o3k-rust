#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script_path="${repo_root}/tests/$(basename "${BASH_SOURCE[0]}")"
artifact="${repo_root}/docs/compatibility/backlog-inventory.yaml"
if [[ "${1:-}" == "--artifact" ]]; then
  artifact="${2:?missing artifact path}"
fi

python3 - "${artifact}" <<'PY'
import json
import re
import sys

artifact_path = sys.argv[1]
doc = json.loads(open(artifact_path, encoding="utf-8").read())

assert doc["schema_version"] == 1
assert doc["format"] == "json-compatible-yaml-1.2"
assert re.fullmatch(r"[0-9a-f]{40}", doc["go_reference"]["commit"]), "go commit must be a pinned 40-hex sha"
assert doc["go_reference"]["repository"] == "https://github.com/kubedoio/o3k"
assert re.fullmatch(r"[0-9a-f]{40}", doc["rust_reference"]["commit"]), "rust commit must be a pinned 40-hex sha"
assert doc["rust_reference"]["repository"] == "https://github.com/kubedoio/o3k-rust"

valid_profiles = {"openstack-service-testbed", "native-rust-testlab", "small-edge-cloud"}
valid_status = {"missing", "partial", "implemented", "unsupported"}
valid_priority = {"blocks-declared-journey", "useful-later", "intentionally-omitted"}
url_re = re.compile(r"^https?://")

all_ids = []
total = 0
for journey in doc["journeys"]:
    journey_id = journey["id"]
    assert journey_id and journey["name"] and journey["client"]
    assert isinstance(journey["recommendation"], str) and journey["recommendation"]
    candidates = journey["candidates"]
    assert candidates, f"{journey_id} has no candidates"
    for candidate in candidates:
        total += 1
        cid = candidate["id"]
        assert cid.startswith(journey_id + "-"), f"{cid} does not use the {journey_id} prefix"
        all_ids.append(cid)
        assert candidate["user_outcome"], f"{cid} missing user_outcome"
        assert candidate["client_command"], f"{cid} missing client_command"
        sources = candidate["official_sources"]
        assert sources and all(url_re.match(s) for s in sources), f"{cid} official_sources must be http(s) URLs"
        go_paths = candidate["go_paths_consulted"]
        assert go_paths and all(isinstance(p, str) and p for p in go_paths), f"{cid} go_paths_consulted"
        assert candidate["rust_status"] in valid_status, f"{cid} rust_status"
        assert isinstance(candidate["in_product_profile"], bool), f"{cid} in_product_profile"
        assert candidate["failure_seen"], f"{cid} failure_seen"
        if candidate["failure_seen"] != "not-exercised":
            assert candidate.get("failure_evidence"), f"{cid} failure_seen without failure_evidence"
        assert candidate["priority"] in valid_priority, f"{cid} priority"
        requires = candidate["requires_before_implementation"]
        assert isinstance(requires, list) and requires, f"{cid} requires_before_implementation"
        assert all(isinstance(r, str) and r for r in requires), f"{cid} requires entries"
        if candidate["in_product_profile"]:
            profiles = candidate.get("profiles")
            assert profiles, f"{cid} in_product_profile without profiles"
            assert set(profiles) <= valid_profiles, f"{cid} unknown profile"
        else:
            assert "profiles" not in candidate, f"{cid} profiles set while in_product_profile is false"

assert len(all_ids) == len(set(all_ids)), "duplicate candidate id"
print(f"validated backlog inventory: {len(doc['journeys'])} journeys, {total} candidates")
PY

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-backlog.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT
cp -- "${artifact}" "${temp_dir}/mutated.yaml"
python3 - "${temp_dir}/mutated.yaml" <<'PY'
import json
import sys

path = sys.argv[1]
data = json.loads(open(path, encoding="utf-8").read())
data["journeys"][0]["candidates"][0]["priority"] = "made-up-priority"
open(path, "w", encoding="utf-8").write(json.dumps(data, indent=2) + "\n")
PY
if bash "${script_path}" --artifact "${temp_dir}/mutated.yaml" >/dev/null 2>&1; then
  echo "backlog validator accepted an invalid priority" >&2
  exit 1
fi
echo "mutation rejected"
