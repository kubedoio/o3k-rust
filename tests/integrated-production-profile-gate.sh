#!/usr/bin/env bash
set -euo pipefail

# Issue #907 integrated production-profile gate.
#
# This is intentionally fail-closed.  The portable portion proves the real
# o3kd process/composition and durable SQLite contracts; it is not production
# evidence.  Setting O3K_INTEGRATED_GATE_REAL=1 opts into the real prerequisite
# checks and PostgreSQL/provider process tests.  Missing prerequisites produce
# BLOCKED (exit 2), never a synthetic PASS.

root_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root_dir"

echo "[integrated-gate] portable process and native contract evidence"
cargo test -p o3kd --all-features --test p12_6_process
# P12.7 is the portable black-box convergence suite.  Its SQLite path proves
# the native and selected compatibility adapters share durable authority,
# rejects cross-project reads/mutations, binds cursors to scope/query, and
# reconstructs the HTTP surfaces after a process/store restart.  Keep this on
# the portable leg; it is evidence of the composition, not PostgreSQL or
# real-provider evidence.
cargo test -p o3kd --all-features --test p12_7_convergence
cargo test -p o3k-native-api --all-features

if [[ "${O3K_INTEGRATED_GATE_REAL:-0}" != 1 ]]; then
  echo "[integrated-gate] PASS: portable integrated composition only"
  echo "[integrated-gate] NOT CLAIMED: HTTPS, PostgreSQL, or real provider"
  exit 0
fi

blocked=0
require_command() { command -v "$1" >/dev/null || { echo "[integrated-gate] BLOCKED: missing command $1" >&2; blocked=1; }; }
require_env() { [[ -n "${!1:-}" ]] || { echo "[integrated-gate] BLOCKED: missing $1" >&2; blocked=1; }; }

require_command curl
require_command pg_isready
require_command python3
require_command timeout
require_env O3K_DATABASE_URL
require_env O3K_INTEGRATED_GATE_URL
require_env O3K_PROVIDER_BINARY
require_env O3K_PROVIDER_VERSION
require_env O3K_INTEGRATED_GATE_EVIDENCE

if [[ -n "${O3K_INTEGRATED_GATE_URL:-}" ]]; then
  if ! python3 - "$O3K_INTEGRATED_GATE_URL" <<'PY'
from urllib.parse import urlparse
import sys
value = sys.argv[1]
parsed = urlparse(value)
if (parsed.scheme != "https" or not parsed.hostname or parsed.username or
        parsed.password or parsed.fragment or any(ch.isspace() for ch in value)):
    raise SystemExit("O3K_INTEGRATED_GATE_URL must be HTTPS with a host and no embedded credentials")
PY
  then
    echo "[integrated-gate] BLOCKED: O3K_INTEGRATED_GATE_URL must be HTTPS without embedded credentials" >&2
    blocked=1
  fi
fi

if [[ -n "${O3K_DATABASE_URL:-}" ]]; then
  if ! python3 - "$O3K_DATABASE_URL" <<'PY'
from urllib.parse import urlparse
import sys
value = sys.argv[1]
parsed = urlparse(value)
# The real profile explicitly proves PostgreSQL behavior.  Do not allow a
# SQLite/file URL to masquerade as production database evidence.
if parsed.scheme not in ("postgres", "postgresql") or not parsed.hostname:
    raise SystemExit("O3K_DATABASE_URL must be a PostgreSQL URL with a host")
PY
  then
    echo "[integrated-gate] BLOCKED: O3K_DATABASE_URL must be a PostgreSQL URL" >&2
    blocked=1
  fi
fi

if [[ -n "${O3K_PROVIDER_BINARY:-}" && ( ! -f "$O3K_PROVIDER_BINARY" || -L "$O3K_PROVIDER_BINARY" || ! -x "$O3K_PROVIDER_BINARY" ) ]]; then
  echo "[integrated-gate] BLOCKED: O3K_PROVIDER_BINARY must be a regular, non-symlink executable" >&2
  blocked=1
fi
if [[ -n "${O3K_PROVIDER_BINARY:-}" && -f "$O3K_PROVIDER_BINARY" ]]; then
  # A symlinked parent can redirect an apparently regular binary after the
  # preflight.  Reject every path component to keep the tested artifact
  # stable for the version check and protected workflow.
  provider_path="$O3K_PROVIDER_BINARY"
  provider_dir="$(dirname -- "$provider_path")"
  while [[ "$provider_dir" != "/" && "$provider_dir" != "." ]]; do
    if [[ -L "$provider_dir" ]]; then
      echo "[integrated-gate] BLOCKED: O3K_PROVIDER_BINARY path contains a symlink" >&2
      blocked=1
      break
    fi
    provider_dir="$(dirname -- "$provider_dir")"
  done
fi

if [[ "$blocked" != 0 ]]; then
  echo "[integrated-gate] BLOCKED: real prerequisites are incomplete" >&2
  exit 2
fi

# The executable checks below are necessary but cannot, by themselves, prove
# the complete #907 journey (for example federated login and cross-surface
# concealment).  Require a signed-by-the-runner evidence manifest from the
# protected production workflow.  Do not turn mere file presence into proof:
# validate every criterion, its production profile, and a non-empty evidence
# reference before running any real-profile checks.
if ! python3 - "$O3K_INTEGRATED_GATE_EVIDENCE" <<'PY'
import json
import pathlib
import sys

required = {
    "federated_auth_context",
    "service_resource_schema_location_discovery",
    "bounded_native_resource_crud",
    "native_compute_lifecycle",
    "canonical_operations",
    "canonical_relationships",
    "authoritative_quota_enforcement",
    "iam_governance",
    "durable_audit_restart",
    "operator_diagnostics",
    "authoritative_metering",
    "cross_project_isolation",
    "secret_non_disclosure",
    "process_restart_recovery",
    "database_bounded_collections",
    "openstack_convergence",
}
manifest_input = pathlib.Path(sys.argv[1])
if manifest_input.is_symlink() or not manifest_input.is_file():
    raise SystemExit("evidence manifest must be a regular non-symlink file")
def reject_symlink_components(path, label):
    # Checking only the leaf is insufficient: a symlinked parent can redirect
    # the artifact after validation.  Work from an absolute lexical path so
    # every directory component, including the manifest's parent, is checked.
    absolute = path if path.is_absolute() else pathlib.Path.cwd() / path
    current = pathlib.Path(absolute.anchor)
    for component in absolute.parts[1:]:
        current /= component
        if current.is_symlink():
            raise SystemExit(f"{label} path contains a symlink")

reject_symlink_components(manifest_input, "evidence manifest")
def reject_writable(path, label):
    try:
        metadata = path.stat()
    except OSError as exc:
        raise SystemExit(f"cannot stat {label}: {exc}")
    # Protected workflow artifacts must not be mutable by another local user
    # after validation.  CI umasks may vary, so only reject group/world write.
    if metadata.st_mode & 0o022:
        raise SystemExit(f"{label} must not be group- or world-writable")

reject_writable(manifest_input, "evidence manifest")
try:
    if manifest_input.stat().st_size > 4 * 1024 * 1024:
        raise SystemExit("evidence manifest exceeds the 4 MiB bound")
except OSError as exc:
    raise SystemExit(f"cannot stat evidence manifest: {exc}")
manifest_path = manifest_input.resolve()
try:
    def reject_duplicate_keys(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError(f"duplicate JSON key: {key}")
            result[key] = value
        return result

    def reject_non_finite(value):
        raise ValueError("non-finite JSON number")

    # Duplicate keys otherwise have ambiguous meaning (Python would silently
    # keep the last value), allowing a signer/verifier or two JSON parsers to
    # observe different criteria.  Non-finite numbers are not JSON and must
    # not be accepted as implementation-specific extensions.
    document = json.loads(
        manifest_path.read_text(),
        object_pairs_hook=reject_duplicate_keys,
        parse_constant=reject_non_finite,
    )
except (OSError, json.JSONDecodeError) as exc:
    raise SystemExit(f"invalid evidence manifest: {exc}")
except ValueError as exc:
    raise SystemExit(f"invalid evidence manifest: {exc}")
if not isinstance(document, dict):
    raise SystemExit("evidence manifest root must be a JSON object")
if document.get("schema_version") != 1 or document.get("profile") != "production":
    raise SystemExit("evidence manifest must declare schema_version=1 and profile=production")
criteria = document.get("criteria")
if not isinstance(criteria, dict) or set(criteria) != required:
    missing = sorted(required - set(criteria or {}))
    extra = sorted(set(criteria or {}) - required)
    raise SystemExit(f"evidence criteria mismatch; missing={missing}, extra={extra}")
for name, item in criteria.items():
    if not isinstance(item, dict) or item.get("status") != "passed":
        raise SystemExit(f"criterion {name} is not passed")
    reference = item.get("evidence")
    if not isinstance(reference, str) or not reference.strip():
        raise SystemExit(f"criterion {name} has no evidence reference")
    value = reference.strip()
    if any(ch.isspace() for ch in value) or value.startswith("http://"):
        raise SystemExit(f"criterion {name} has an invalid evidence reference")
    if value.startswith("https://"):
        from urllib.parse import urlparse
        parsed = urlparse(value)
        if (not parsed.netloc or parsed.username or parsed.password or
                parsed.fragment):
            raise SystemExit(f"criterion {name} has an invalid HTTPS evidence reference")
    else:
        # Evidence must be a regular artifact, never a directory, device, or
        # symlink whose target can change after manifest validation.  Relative
        # paths are intentionally anchored to the manifest directory so a
        # manifest cannot reach arbitrary workspace files via traversal.
        artifact = pathlib.Path(value)
        # Evidence is runner-local and must live below the manifest directory.
        # Reject absolute paths and traversal/symlink escapes: otherwise a
        # manifest could bless arbitrary host files as production evidence.
        if artifact.is_absolute():
            raise SystemExit(f"criterion {name} evidence path must be relative to the manifest")
        artifact = manifest_path.parent / artifact
        reject_symlink_components(artifact, f"criterion {name} evidence")
        try:
            resolved = artifact.resolve(strict=True)
        except OSError:
            raise SystemExit(f"criterion {name} evidence artifact is missing")
        try:
            resolved.relative_to(manifest_path.parent.resolve())
        except ValueError:
            raise SystemExit(f"criterion {name} evidence artifact escapes manifest directory")
        # Reject symlinks in every path component, not only the leaf.  This
        # keeps an artifact from changing after validation through a mutable
        # directory link while still allowing ordinary nested artifact paths.
        current = manifest_path.parent
        for component in artifact.relative_to(manifest_path.parent).parts:
            current = current / component
            if current.is_symlink():
                raise SystemExit(f"criterion {name} evidence path contains a symlink")
        # A small manifest must not be able to make the protected workflow
        # ingest an unbounded local evidence blob.  The artifact remains an
        # opaque reference here; the protected workflow owns stronger format
        # and provenance checks.
        artifact_size = resolved.stat().st_size
        if not artifact.is_file() or artifact_size == 0:
            raise SystemExit(f"criterion {name} evidence artifact is missing or empty")
        if artifact_size > 64 * 1024 * 1024:
            raise SystemExit(f"criterion {name} evidence artifact exceeds the 64 MiB bound")
        reject_writable(resolved, f"criterion {name} evidence artifact")
print("validated production evidence manifest: 16 #907 criteria")
PY
then
  echo "[integrated-gate] BLOCKED: invalid or incomplete production evidence manifest" >&2
  exit 2
fi

pg_isready --timeout=5 "$O3K_DATABASE_URL" >/dev/null || { echo "[integrated-gate] BLOCKED: PostgreSQL is not ready" >&2; exit 2; }
curl --fail --silent --show-error --connect-timeout 5 --max-time 15 "$O3K_INTEGRATED_GATE_URL/health" >/dev/null || {
  echo "[integrated-gate] BLOCKED: HTTPS o3kd health probe failed" >&2; exit 2;
}

provider_version="$(timeout --kill-after=2s 10s "$O3K_PROVIDER_BINARY" --version 2>/dev/null || true)"
grep -F "$O3K_PROVIDER_VERSION" <<<"$provider_version" >/dev/null || {
  echo "[integrated-gate] BLOCKED: provider version mismatch (expected $O3K_PROVIDER_VERSION)" >&2
  exit 2
}

echo "[integrated-gate] PostgreSQL store conformance"
cargo test -p o3k-store --all-features --test postgres_p12_4 -- --ignored
echo "[integrated-gate] PostgreSQL native/compatibility convergence"
# This suite exercises both HTTP adapters against one durable PostgreSQL
# authority, including restart reconstruction and cross-surface convergence.
# It is intentionally kept on the real path: SQLite/portable results cannot
# be promoted to PostgreSQL production evidence.
cargo test -p o3kd --all-features --test p12_7_convergence -- --ignored --nocapture

echo "[integrated-gate] native contract closure regression gates"
# These are repository contract gates (not a provider or IdP substitute), but
# must remain part of every real-profile run so a deployed binary cannot pass
# the infrastructure checks with stale public metadata.
bash tests/native-closure-gate.sh
bash tests/native-iam-contract.sh

echo "[integrated-gate] real prerequisites and PostgreSQL convergence passed"
echo "[integrated-gate] NOT CLAIMED: complete 16-point #907 release gate,
real provider lifecycle, or production Araf TLS/IdP evidence"
# This script only validates deployment prerequisites and repository/database
# conformance.  It deliberately does not execute the protected host workflow
# that proves the 16 criteria, so a successful prerequisite run must never be
# usable as a release signal by callers that key on the exit status.
exit 2
