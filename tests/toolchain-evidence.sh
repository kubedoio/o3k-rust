#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
expected_rust="1.97.1"
toolchain_file="${repo_root}/rust-toolchain.toml"
lockfile="${repo_root}/Cargo.lock"

grep -Fqx 'channel = "1.97.1"' "${toolchain_file}"
grep -Fqx 'rust-version = "1.97.1"' "${repo_root}/Cargo.toml"
test -s "${lockfile}"

rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
case "${rustc_version}" in
    "rustc ${expected_rust} ("*) ;;
    *) echo "unexpected rustc: ${rustc_version}" >&2; exit 1 ;;
esac
case "${cargo_version}" in
    "cargo ${expected_rust} ("*) ;;
    *) echo "unexpected cargo: ${cargo_version}" >&2; exit 1 ;;
esac

# Metadata must be generated from the lockfile; this command is the reproducible
# check and intentionally does not publish a release or compatibility claim.
cargo metadata --locked --format-version 1 >/dev/null

python3 - "${repo_root}" "${rustc_version}" "${cargo_version}" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys

root = pathlib.Path(sys.argv[1])
def sha256(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()

record = {
    "evidence_state": "portable-contract-verified",
    "claim": "the repository uses the exact selected Rust toolchain and lockfile",
    "source_commit": subprocess.check_output(
        ["git", "-C", str(root), "rev-parse", "HEAD"], text=True
    ).strip(),
    "rustc": sys.argv[2],
    "cargo": sys.argv[3],
    "toolchain_sha256": sha256(root / "rust-toolchain.toml"),
    "lockfile_sha256": sha256(root / "Cargo.lock"),
    "command": "cargo metadata --locked --format-version 1",
    "protected_runner": False,
    "release": False,
}
assert record["evidence_state"] == "portable-contract-verified"
assert not record["protected_runner"] and not record["release"]
print(json.dumps(record, sort_keys=True))
PY
