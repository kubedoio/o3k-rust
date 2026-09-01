#!/usr/bin/env bash
set -euo pipefail

root_dir=$(cd "$(dirname "$0")/.." && pwd)
o3kd=${O3K_P13_O3KD:-$root_dir/target/debug/o3kd}
: "${O3K_LVM_VOLUME_GROUP:?set a disposable LVM volume group}"
: "${O3K_LVM_THIN_POOL:?set a disposable LVM thin pool}"
: "${O3K_LVM_PROVIDER_NAMESPACE:?set a disposable LVM provider namespace}"
password=${O3K_P13_PASSWORD:-p13-4-disposable-password}
project_id=eba29e2d-53de-461d-ae91-ede7402713cb
port=$(python3 - <<'PY'
import socket
s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()
PY
)
work=$(mktemp -d /tmp/o3k-p13-4.XXXXXX)
pid=
cleanup() { if [[ -n "$pid" ]]; then kill "$pid" 2>/dev/null || true; wait "$pid" 2>/dev/null || true; fi; rm -rf "$work"; }
trap cleanup EXIT

O3K_BOOTSTRAP_PASSWORD="$password" \
O3K_TOKEN_SIGNING_KEY="p13-4-storage-token-signing-key-012345678901234567890123" \
O3K_LVM_VOLUME_GROUP="$O3K_LVM_VOLUME_GROUP" \
O3K_LVM_THIN_POOL="$O3K_LVM_THIN_POOL" \
O3K_LVM_PROVIDER_NAMESPACE="$O3K_LVM_PROVIDER_NAMESPACE" \
  "$o3kd" --listen-addr "127.0.0.1:$port" --data-dir "$work/data" >"$work/o3kd.log" 2>&1 &
pid=$!
for _ in $(seq 1 120); do
  curl -fsS "http://127.0.0.1:$port/readyz" >/dev/null 2>&1 && break
  sleep 0.1
done

curl -fsS -D "$work/auth.headers" -o "$work/auth.json" \
  -H 'Content-Type: application/json' -X POST "http://127.0.0.1:$port/v3/auth/tokens" \
  --data "{\"auth\":{\"identity\":{\"methods\":[\"password\"],\"password\":{\"user\":{\"name\":\"admin\",\"password\":\"$password\"}}},\"scope\":{\"project\":{\"name\":\"admin\"}}}}"
token=$(awk 'tolower($1)=="x-subject-token:" {print $2}' "$work/auth.headers" | tr -d '\r')

create=$(curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' \
  -X POST "http://127.0.0.1:$port/v3/$project_id/volumes" \
  --data '{"volume":{"size":1,"name":"p13-4-volume"}}')
volume_id=$(python3 -c 'import json,sys; print(json.load(sys.stdin)["volume"]["id"])' <<<"$create")
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v3/$project_id/volumes/$volume_id" >/dev/null
curl -fsS -H "X-Auth-Token: $token" "http://127.0.0.1:$port/v3/$project_id/volumes" >/dev/null
curl -fsS -H "X-Auth-Token: $token" -H 'Content-Type: application/json' \
  -X PUT "http://127.0.0.1:$port/v3/$project_id/volumes/$volume_id" \
  --data '{"volume":{"description":"updated"}}' >/dev/null
curl -fsS -o /dev/null -w '%{http_code}' -H "X-Auth-Token: $token" \
  -X DELETE "http://127.0.0.1:$port/v3/$project_id/volumes/$volume_id" | grep -qx 204
echo "P13.4 native Cinder volume lifecycle passed"
