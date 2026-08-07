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
status = "proposed"
normative_adr = "fixture"
normative_spec = "fixture"
application_crates = ["o3k-app"]

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

# The exact current debt fixture is valid.
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

# Remove the forbidden dependency and add concrete-store coupling in a new file.
sed -i '/^sqlx =/d' "${temp_dir}/crates/o3k-app/Cargo.toml"
printf '%s\n' 'struct SqliteStore;' > "${temp_dir}/crates/o3k-app/src/new_debt.rs"
if python3 scripts/check-architecture-boundaries.py --root "${temp_dir}" >/dev/null 2>&1; then
  echo "ERROR: architecture checker accepted new concrete-store debt outside the exact allowlist" >&2
  exit 1
fi

echo "Architecture boundary ratchet and negative tests passed"
