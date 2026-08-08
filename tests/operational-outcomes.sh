#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
script_path="${repo_root}/tests/$(basename "${BASH_SOURCE[0]}")"
artifact="${repo_root}/docs/operations/operational-outcomes-inventory.yaml"
pinned_go_commit="53fd2cb36ee79f42da49c8181d6ceed12b41b3aa"
if [[ "${1:-}" == "--artifact" ]]; then
  artifact="${2:?missing artifact path}"
fi

# 1. Inventory schema validation (JSON-compatible YAML; fail closed on drift).
python3 - "${artifact}" "${pinned_go_commit}" <<'PY'
import json
import sys

artifact_path, pinned_go_commit = sys.argv[1:]

required_fields = [
    "id", "name", "requirement", "go_behavior", "go_paths_consulted",
    "rust_status", "rust_owner", "profile", "evidence", "gap",
    "priority", "requires_before_implementation",
]
valid_status = {"implemented", "partial", "missing"}
valid_priority = {"blocks-declared-journey", "useful-later", "intentionally-omitted"}
valid_profile = "native-rust-testlab"

doc = json.loads(open(artifact_path, encoding="utf-8").read())

assert doc["schema_version"] == 1, "schema_version must be 1"
assert doc["profile"] == valid_profile, "top-level profile must be native-rust-testlab"
assert doc["go_reference"]["repo"] == "https://github.com/kubedoio/o3k", "go_reference.repo"
assert doc["go_reference"]["commit"] == pinned_go_commit, (
    f"go_reference.commit drifted: expected {pinned_go_commit}, "
    f"got {doc['go_reference']['commit']}"
)

outcomes = doc["outcomes"]
expected_ids = [f"OP-{i:03d}" for i in range(1, 11)]
assert [o["id"] for o in outcomes] == expected_ids, (
    "outcomes must be exactly OP-001..OP-010 in order"
)


def require_str(oid, field, value):
    assert isinstance(value, str) and value, f"{oid} {field}"


def require_strs(oid, field, values):
    assert values and all(isinstance(v, str) and v for v in values), f"{oid} {field}"


for outcome in outcomes:
    oid = outcome["id"]
    for field in required_fields:
        assert field in outcome, f"{oid} missing field {field}"
    for field in ("name", "requirement", "go_behavior", "rust_owner", "gap"):
        require_str(oid, field, outcome[field])
    for field in ("go_paths_consulted", "evidence", "requires_before_implementation"):
        require_strs(oid, field, outcome[field])
    assert outcome["rust_status"] in valid_status, f"{oid} rust_status {outcome['rust_status']!r}"
    assert outcome["profile"] == valid_profile, f"{oid} profile"
    assert outcome["priority"] in valid_priority, f"{oid} priority {outcome['priority']!r}"

assert isinstance(doc.get("notes"), list) and doc["notes"], "notes must be a non-empty list"
print(f"validated operational-outcomes inventory: {len(outcomes)} outcomes, commit {pinned_go_commit}")
PY

# 2. Static source-bound checks: every cited path must exist (Go provenance
#    paths when the pinned reference checkout is present; CI does not clone it).
python3 - "${artifact}" "${repo_root}" <<'PY'
import json
import os
import sys

doc = json.loads(open(sys.argv[1], encoding="utf-8").read())
root = sys.argv[2]
missing = [
    f"evidence {p}"
    for o in doc["outcomes"]
    for p in o["evidence"]
    if not os.path.exists(os.path.join(root, p))
]
go_root = os.path.join(root, "target/go-o3k-reference")
if os.path.isdir(go_root):
    missing += [
        f"go_paths_consulted {p}"
        for o in doc["outcomes"]
        for p in o["go_paths_consulted"]
        if not os.path.exists(os.path.join(go_root, p))
    ]
if missing:
    print("operational outcomes: missing paths:\n" + "\n".join(missing), file=sys.stderr)
    sys.exit(1)
PY

check_unit() {
  local unit="$1"
  if ! grep -q '^EnvironmentFile=' "${repo_root}/${unit}"; then
    echo "operational outcomes: ${unit} lost its EnvironmentFile= line" >&2
    exit 1
  fi
  if grep -q '^Environment=' "${repo_root}/${unit}"; then
    echo "operational outcomes: ${unit} gained a secret-bearing Environment= line" >&2
    exit 1
  fi
}
check_unit packaging/o3kd.service
check_unit packaging/o3k-compute.service

grep -q '\.o3k-owned' "${repo_root}/packaging/install.sh" || {
  echo "operational outcomes: install.sh no longer writes .o3k-owned markers" >&2; exit 1; }
grep -q '\.o3k-installed' "${repo_root}/packaging/install.sh" || {
  echo "operational outcomes: install.sh no longer writes the .o3k-installed manifest" >&2; exit 1; }
grep -q 'systemctl enable' "${repo_root}/packaging/install.sh" || {
  echo "operational outcomes: install.sh no longer enables the service units" >&2; exit 1; }
grep -q -- '--yes' "${repo_root}/packaging/reset.sh" || {
  echo "operational outcomes: reset.sh no longer requires --yes" >&2; exit 1; }
grep -q 'unowned' "${repo_root}/packaging/reset.sh" || {
  echo "operational outcomes: reset.sh no longer refuses unowned directories" >&2; exit 1; }
grep -q '\.o3k-installed' "${repo_root}/packaging/uninstall.sh" || {
  echo "operational outcomes: uninstall.sh no longer refuses without the install manifest" >&2; exit 1; }
grep -q 'umask 077' "${repo_root}/scripts/generate-passwords.sh" || {
  echo "operational outcomes: generate-passwords.sh no longer sets umask 077" >&2; exit 1; }
grep -q '0600' "${repo_root}/scripts/generate-passwords.sh" || {
  echo "operational outcomes: generate-passwords.sh no longer writes mode 0600" >&2; exit 1; }
grep -q 'flock' "${repo_root}/scripts/generate-passwords.sh" || {
  echo "operational outcomes: generate-passwords.sh no longer uses flock" >&2; exit 1; }
grep -q 'identity is not configured' "${repo_root}/bins/o3kd/src/main.rs" || {
  echo "operational outcomes: o3kd no longer emits the identity warning marker" >&2; exit 1; }
grep -q 'tests/operational-outcomes.sh' "${repo_root}/.github/workflows/ci.yml" || {
  echo "operational outcomes: ci.yml no longer runs tests/operational-outcomes.sh" >&2; exit 1; }
grep -q 'tests/packaging-sbom.sh' "${repo_root}/.github/workflows/ci.yml" || {
  echo "operational outcomes: ci.yml no longer runs tests/packaging-sbom.sh" >&2; exit 1; }
grep -q 'must not be described as' "${repo_root}/docs/RELEASE.md" || {
  echo "operational outcomes: docs/RELEASE.md no longer contains the no-signing claim" >&2; exit 1; }
grep -q 'signed merely because it contains checksums' "${repo_root}/docs/RELEASE.md" || {
  echo "operational outcomes: docs/RELEASE.md no longer explains checksums are not signatures" >&2; exit 1; }

# Mutation check: the validator must reject an invalid rust_status.
temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-operational.XXXXXX")"
cp -- "${artifact}" "${temp_dir}/mutated.yaml"
python3 - "${temp_dir}/mutated.yaml" <<'PY'
import json
import sys

path = sys.argv[1]
data = json.loads(open(path, encoding="utf-8").read())
data["outcomes"][0]["rust_status"] = "invented-status"
open(path, "w", encoding="utf-8").write(json.dumps(data, indent=2) + "\n")
PY
if bash "${script_path}" --artifact "${temp_dir}/mutated.yaml" >/dev/null 2>&1; then
  echo "operational outcomes: validator accepted an invalid rust_status" >&2
  exit 1
fi
echo "mutation rejected"

# 3. Runtime check: o3kd must boot without identity environment variables,
#    warn that identity is not configured, and answer /healthz and /readyz.
if [[ ! -x "${repo_root}/target/debug/o3kd" ]]; then
  echo "operational outcomes: target/debug/o3kd is missing; run 'cargo build --bin o3kd' first" >&2
  exit 2
fi

data_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-operational-data.XXXXXX")"
log_file="$(mktemp "${TMPDIR:-/tmp}/o3k-operational-log.XXXXXX")"
o3kd_pid=""
cleanup() {
  set +e
  if [[ -n "${o3kd_pid}" ]]; then kill -TERM "${o3kd_pid}" 2>/dev/null; wait "${o3kd_pid}" 2>/dev/null; fi
  rm -rf -- "${temp_dir}" "${data_dir}"
  rm -f -- "${log_file}"
}
trap cleanup EXIT

# Derive a high port from the shell pid; fail closed if something listens there.
port=$((19000 + ($$ % 5000)))
if bash -c "exec 3<>/dev/tcp/127.0.0.1/${port}" 2>/dev/null; then
  echo "operational outcomes: port ${port} is already occupied; refusing to run" >&2
  exit 2
fi

env -u O3K_BOOTSTRAP_PASSWORD -u O3K_TOKEN_SIGNING_KEY -u O3K_BOOTSTRAP_SECRET \
  "${repo_root}/target/debug/o3kd" --listen-addr "127.0.0.1:${port}" \
  --data-dir "${data_dir}" --log-filter warn >"${log_file}" 2>&1 &
o3kd_pid=$!

ready=0
for _ in $(seq 1 300); do
  if curl -fsS "http://127.0.0.1:${port}/healthz" >/dev/null 2>&1; then
    ready=1
    break
  fi
  kill -0 "${o3kd_pid}" 2>/dev/null || break
  sleep 0.1
done
if [[ "${ready}" != 1 ]]; then
  echo "operational outcomes: o3kd did not answer GET /healthz within 30s" >&2
  sed -n '1,50p' "${log_file}" >&2 || true
  exit 1
fi
grep -q 'identity is not configured' "${log_file}" || {
  echo "operational outcomes: identity warning marker missing from o3kd startup logs" >&2
  sed -n '1,50p' "${log_file}" >&2 || true
  exit 1
}
curl -fsS "http://127.0.0.1:${port}/readyz" >/dev/null || {
  echo "operational outcomes: GET /readyz did not return 200" >&2
  exit 1
}
echo "operational outcomes: o3kd booted without identity env; warning emitted; /healthz and /readyz answered 200"
