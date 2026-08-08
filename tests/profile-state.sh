#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

echo "Running product-profile status governance validator..."
python3 "${repo_root}/scripts/validate-profile-state.py" --root "${repo_root}"

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-profile-state.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT

# Mutate a copy of the real status file; every mutation must be rejected.
mutate() {
  local target="$1"
  shift
  local mutation_name="$1"
  python3 - "${repo_root}/docs/status/current-state.yaml" "${target}" "${mutation_name}" <<'PY'
import sys
import yaml

source, target, mutation_name = sys.argv[1], sys.argv[2], sys.argv[3]
with open(source, encoding="utf-8") as handle:
    doc = yaml.safe_load(handle)
mutations = {
    "rename-profile": lambda d: d["profiles"].__setitem__(
        "native-rust-testlab-x", d["profiles"].pop("native-rust-testlab")
    ),
    "missing-field": lambda d: d["profiles"]["native-rust-testlab"].pop(
        "explicitly_unproven_claims"
    ),
    "cinder-evidence-in-native": lambda d: d["profiles"]["native-rust-testlab"][
        "portable_evidence"
    ].append({"name": "real-cinder-lifecycle", "state": "passed"}),
    "native-full-profile-passed": lambda d: d["profiles"]["native-rust-testlab"][
        "full_profile_evidence"
    ][0].__setitem__("state", "passed"),
    "bad-evidence-state": lambda d: d["profiles"]["native-rust-testlab"][
        "portable_evidence"
    ][0].__setitem__("state", "banana"),
    "bad-source-commit": lambda d: d["profiles"]["native-rust-testlab"].__setitem__(
        "source_commit", "0" * 40
    ),
}
mutations[mutation_name](doc)
with open(target, "w", encoding="utf-8") as handle:
    yaml.safe_dump(doc, handle, sort_keys=False)
PY
}

for mutation in rename-profile missing-field cinder-evidence-in-native \
    native-full-profile-passed bad-evidence-state bad-source-commit; do
  mutated="${temp_dir}/status-${mutation}.yaml"
  mutate "${mutated}" "${mutation}"
  if python3 "${repo_root}/scripts/validate-profile-state.py" \
      --root "${repo_root}" --status-file "${mutated}" >/dev/null 2>&1; then
    echo "ERROR: validator accepted mutated status (${mutation})" >&2
    exit 1
  fi
  echo "mutation rejected: ${mutation}"
done

echo "Product-profile status governance tests passed"
