#!/usr/bin/env bash
set -euo pipefail

container="o3k-audit-postgres-${RANDOM}-${RANDOM}"
password="o3k-audit-disposable"
cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT

docker run --rm -d --name "$container" \
  -e POSTGRES_USER=o3k -e POSTGRES_PASSWORD="$password" -e POSTGRES_DB=o3k_test \
  -p 127.0.0.1::5432 postgres:16.4 >/dev/null

port=""
for _ in $(seq 1 60); do
  port="$(docker port "$container" 5432/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1)"
  if [[ -n "$port" ]] && PGPASSWORD="$password" pg_isready -h 127.0.0.1 -p "$port" -U o3k -d o3k_test >/dev/null 2>&1; then break; fi
  sleep 1
done
if [[ -z "$port" ]] || ! PGPASSWORD="$password" pg_isready -h 127.0.0.1 -p "$port" -U o3k -d o3k_test >/dev/null 2>&1; then
  echo "PostgreSQL readiness timeout (container=$container)" >&2
  docker logs "$container" >&2 || true
  exit 1
fi

export O3K_DATABASE_URL="postgres://o3k:${password}@127.0.0.1:${port}/o3k_test"
cargo test -p o3k-store --test postgres_audit_repository --all-features -- --nocapture
