#!/usr/bin/env bash
set -euo pipefail

# Portable, repository-local evidence gate for the native northbound closure.
#
# This gate deliberately exercises the production Rust composition and its
# durable repositories through their test harnesses.  It does not use the
# OpenStack compatibility fixture harness, browser data, provider CLIs, or
# an external identity provider.  Consequently a successful run is portable
# composition evidence, not a real-federated-IDP or real-host release claim.

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "${repo_root}"

if [[ -n "${O3K_COMPATIBILITY_FIXTURES:-}" ]]; then
  echo "native closure gate refuses compatibility fixtures" >&2
  exit 2
fi

echo "[native-closure] native API contract and route tests"
cargo test -p o3k-native-api --all-features

echo "[native-closure] o3kd production composition and native adapters"
cargo test -p o3kd --all-features native_

echo "[native-closure] durable authority, bounded collections, audit and metering"
cargo test -p o3k-store --all-features

echo "[native-closure] portable native composition evidence passed"
echo "[native-closure] NOT CLAIMED: federated OIDC, Araf process, PostgreSQL deployment, or real-provider evidence"
