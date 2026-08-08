#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

echo "Checking repository architecture boundaries..."
python3 scripts/check-architecture-boundaries.py

temp_dir="$(mktemp -d "${TMPDIR:-/tmp}/o3k-architecture-test.XXXXXX")"
trap 'rm -rf "${temp_dir}"' EXIT

mkdir -p \
  "${temp_dir}/contracts" \
  "${temp_dir}/crates/o3k-domain/src" \
  "${temp_dir}/crates/o3k-app/src"

cat > "${temp_dir}/crates/o3k-domain/Cargo.toml" <<'EOF'
[package]
name = "o3k-domain"
version = "0.0.0"

[dependencies]
serde = "1"
EOF
cat > "${temp_dir}/crates/o3k-domain/src/lib.rs" <<'EOF'
pub struct DomainId(pub String);
EOF

cat > "${temp_dir}/crates/o3k-app/Cargo.toml" <<'EOF'
[package]
name = "o3k-app"
version = "0.0.0"

[dependencies]
o3k-store = { path = "../o3k-store" }
EOF
cat > "${temp_dir}/crates/o3k-app/src/lib.rs" <<'EOF'
// Current explicitly ratcheted debt for the synthetic fixture.
struct SqliteStore;
EOF

cat > "${temp_dir}/contracts/core-architecture-boundaries.toml" <<'EOF'
schema_version = 1
status = "accepted"
normative_adr = "fixture"
normative_spec = "fixture"
application_crates = ["o3k-app"]
non_application_crates = ["o3k-domain"]

[domain]
crate = "o3k-domain"
allowed_dependencies = ["serde"]
forbidden_source_markers = ["sqlx::", "o3k_store"]

[application]
hard_forbidden_dependencies = ["sqlx"]
ratcheted_adapter_dependencies = ["o3k-store"]
concrete_store_symbol = "SqliteStore"
concrete_store_debt_files = ["crates/o3k-app/src/lib.rs"]

[application.adapter_dependency_debt]
o3k-app = ["o3k-store"]

[ratchet]
allow_debt_reduction = true
allow_new_debt = false
allow_broader_debt = false
EOF

# Synthetic debt fixture exercising the checker's adapter_debt machinery with
# enforcement active (status accepted); the real contract no longer uses the
# adapter_dependency_debt section (SPEC-0025 step 6), and the machinery is
# retained so a future reintroduction is still validated.
python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null

# Removing code debt without removing its exception must fail: dormant
# exceptions would otherwise permit a future silent reintroduction.
printf '%s\n' 'pub struct CleanApplication;' > "${temp_dir}/crates/o3k-app/src/lib.rs"
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted a stale concrete-store exception" >&2
  exit 1
fi

# Restore the declared debt, then prove a new hard-forbidden dependency fails.
printf '%s\n' 'struct SqliteStore;' > "${temp_dir}/crates/o3k-app/src/lib.rs"
cat >> "${temp_dir}/crates/o3k-app/Cargo.toml" <<'EOF'
sqlx = "0.8"
EOF
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted a new forbidden application dependency" >&2
  exit 1
fi

# A forbidden dependency hidden under a target-specific table must fail.
sed -i '/^sqlx =/d' "${temp_dir}/crates/o3k-app/Cargo.toml"
cat >> "${temp_dir}/crates/o3k-app/Cargo.toml" <<'EOF'

[target.'cfg(unix)'.dependencies]
sqlx = "0.8"
EOF
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker missed a target-specific forbidden dependency" >&2
  exit 1
fi

# A forbidden dependency hidden behind a rename must fail.
sed -i "/\[target.'cfg(unix)'.dependencies\]/,+1d" "${temp_dir}/crates/o3k-app/Cargo.toml"
cat >> "${temp_dir}/crates/o3k-app/Cargo.toml" <<'EOF'
db = { package = "sqlx", version = "0.8" }
EOF
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker missed a renamed forbidden dependency" >&2
  exit 1
fi

# An unclassified new workspace crate must fail (exhaustive classification).
sed -i '/^db = /d' "${temp_dir}/crates/o3k-app/Cargo.toml"
mkdir -p "${temp_dir}/crates/o3k-newapp/src"
cat > "${temp_dir}/crates/o3k-newapp/Cargo.toml" <<'EOF'
[package]
name = "o3k-newapp"
version = "0.0.0"

[dependencies]
sqlx = "0.8"
EOF
printf '%s\n' 'pub fn leaked() {}' > "${temp_dir}/crates/o3k-newapp/src/lib.rs"
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted an unclassified workspace crate" >&2
  exit 1
fi

# Remove the forbidden dependency and add concrete-store coupling in a new file.
rm -rf "${temp_dir}/crates/o3k-newapp"
printf '%s\n' 'struct SqliteStore;' > "${temp_dir}/crates/o3k-app/src/new_debt.rs"
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted new concrete-store debt outside the exact allowlist" >&2
  exit 1
fi

# Deferred mode: while the contract status is not accepted, boundary enforcement
# is skipped (so CI never rejects a change on a non-accepted decision) but the
# structural checks (including crate classification) still run.
sed -i 's/^status = "accepted"/status = "proposed"/' "${temp_dir}/contracts/core-architecture-boundaries.toml"
cat >> "${temp_dir}/crates/o3k-app/Cargo.toml" <<'EOF'
sqlx = "0.8"
EOF
python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null

# Deferred mode still rejects an unclassified workspace crate (structural check).
mkdir -p "${temp_dir}/crates/o3k-ghost/src"
cat > "${temp_dir}/crates/o3k-ghost/Cargo.toml" <<'EOF'
[package]
name = "o3k-ghost"
version = "0.0.0"
EOF
printf '%s\n' 'pub struct Ghost;' > "${temp_dir}/crates/o3k-ghost/src/lib.rs"
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted an unclassified crate in deferred mode" >&2
  exit 1
fi

echo "Architecture boundary ratchet and negative tests passed"
