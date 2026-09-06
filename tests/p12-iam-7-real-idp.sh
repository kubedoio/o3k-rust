#!/usr/bin/env bash
set -euo pipefail

# Deterministic real-provider evidence for P12-IAM.7. Keycloak is a reference
# provider only; no Keycloak types or claims enter canonical O3K IAM.
command -v docker >/dev/null || { echo "docker is required" >&2; exit 2; }
command -v curl >/dev/null || { echo "curl is required" >&2; exit 2; }
command -v python3 >/dev/null || { echo "python3 is required" >&2; exit 2; }

run_id="o3k-p12-7-${RANDOM}-${RANDOM}"
keycloak_image="quay.io/keycloak/keycloak:25.0.6@sha256:82c5b7a110456dbd42b86ea572e728878549954cc8bd03cd65410d75328095d2"
port="$(python3 - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
)"
workdir="$(mktemp -d)"
container="${run_id}-keycloak"
admin_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
alice_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
bob_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
operator_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
unbound_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
o3k_seed_password="$(python3 -c 'import secrets; print(secrets.token_hex(24))')"
cleanup() {
  docker rm -f "${container}" >/dev/null 2>&1 || true
  rm -rf "${workdir}"
}
trap cleanup EXIT

cat >"${workdir}/p12-7-realm.json" <<JSON
{
  "realm": "o3k-p12-7",
  "enabled": true,
  "clients": [{
    "clientId": "o3k-test",
    "name": "O3K P12.7 test client",
    "enabled": true,
    "publicClient": true,
    "directAccessGrantsEnabled": true,
    "standardFlowEnabled": false,
    "protocol": "openid-connect",
    "protocolMappers": [{
      "name": "o3k-audience",
      "protocol": "openid-connect",
      "protocolMapper": "oidc-audience-mapper",
      "config": {
        "included.client.audience": "o3k",
        "id.token.claim": "false",
        "access.token.claim": "true"
      }
    }]
  }],
  "users": [
    {"username": "alice", "firstName": "Alice", "lastName": "Tenant", "email": "alice@example.test", "enabled": true, "emailVerified": true, "requiredActions": [], "credentials": [{"type": "password", "value": "${alice_password}", "temporary": false}]},
    {"username": "bob", "firstName": "Bob", "lastName": "Tenant", "email": "bob@example.test", "enabled": true, "emailVerified": true, "requiredActions": [], "credentials": [{"type": "password", "value": "${bob_password}", "temporary": false}]},
    {"username": "operator", "firstName": "Cloud", "lastName": "Operator", "email": "operator@example.test", "enabled": true, "emailVerified": true, "requiredActions": [], "credentials": [{"type": "password", "value": "${operator_password}", "temporary": false}]},
    {"username": "unbound", "firstName": "Unbound", "lastName": "User", "email": "unbound@example.test", "enabled": true, "emailVerified": true, "requiredActions": [], "credentials": [{"type": "password", "value": "${unbound_password}", "temporary": false}]}
  ]
}
JSON

docker run -d --name "${container}" --network host \
  -e KEYCLOAK_ADMIN=p12-7-admin \
  -e KEYCLOAK_ADMIN_PASSWORD="${admin_password}" \
  -e KC_HTTP_PORT="${port}" \
  -e KC_HOSTNAME="http://127.0.0.1:${port}" \
  -e KC_HOSTNAME_STRICT=false \
  -v "${workdir}/p12-7-realm.json:/opt/keycloak/data/import/p12-7-realm.json:ro" \
  "${keycloak_image}" start-dev --http-port="${port}" --import-realm \
  >/dev/null

for _ in $(seq 1 90); do
  if curl -fsS "http://127.0.0.1:${port}/realms/o3k-p12-7/.well-known/openid-configuration" \
    -o "${workdir}/discovery.json" 2>/dev/null; then
    break
  fi
  sleep 1
done
test -s "${workdir}/discovery.json"

issuer="$(python3 - "${workdir}/discovery.json" <<'PY'
import json, sys
print(json.load(open(sys.argv[1]))["issuer"])
PY
)"
discovery="http://127.0.0.1:${port}/realms/o3k-p12-7/.well-known/openid-configuration"

# Re-apply passwords through the provider's administrative API after import.
# This keeps the realm export portable across Keycloak minor versions, whose
# imported credential metadata has changed more than once.
admin_token="$(curl -fsS -X POST "http://127.0.0.1:${port}/realms/master/protocol/openid-connect/token" \
  -d grant_type=password -d client_id=admin-cli -d username=p12-7-admin -d password="${admin_password}" \
  | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')"
reset_password() {
  local username="$1" password="$2" user_id
  user_id="$(curl -fsS -H "Authorization: Bearer ${admin_token}" \
    "http://127.0.0.1:${port}/admin/realms/o3k-p12-7/users?username=${username}&exact=true" \
    | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
  curl -fsS -X PUT -H "Authorization: Bearer ${admin_token}" -H 'Content-Type: application/json' \
    "http://127.0.0.1:${port}/admin/realms/o3k-p12-7/users/${user_id}/reset-password" \
    -d "{\"type\":\"password\",\"value\":\"${password}\",\"temporary\":false}" >/dev/null
}
reset_password alice "${alice_password}"
reset_password bob "${bob_password}"
reset_password operator "${operator_password}"
reset_password unbound "${unbound_password}"

token_for() {
  local response
  response="$(curl -sS -X POST "http://127.0.0.1:${port}/realms/o3k-p12-7/protocol/openid-connect/token" \
    -d grant_type=password -d client_id=o3k-test -d username="$1" -d password="$2")"
  python3 -c 'import json,sys
payload=json.loads(sys.argv[1])
if "access_token" not in payload:
    raise SystemExit("Keycloak token exchange failed: " + str(payload.get("error_description", payload.get("error", "unknown error"))))
print(payload["access_token"])' "${response}"
}
subject_for() {
  python3 - "$1" <<'PY'
import base64, json, sys
part = sys.argv[1].split(".")[1]
print(json.loads(base64.urlsafe_b64decode(part + "=" * (-len(part) % 4)))["sub"])
PY
}

alice_token="$(token_for alice "${alice_password}")"
bob_token="$(token_for bob "${bob_password}")"
operator_token="$(token_for operator "${operator_password}")"
unbound_token="$(token_for unbound "${unbound_password}")"
alice_subject="$(subject_for "${alice_token}")"
bob_subject="$(subject_for "${bob_token}")"
operator_subject="$(subject_for "${operator_token}")"

if [[ -z "${O3K_DATABASE_URL:-}" ]]; then
  export O3K_P12_7_SQLITE_PATH="${workdir}/p12-7.sqlite"
fi
export O3K_P12_7_ISSUER="${issuer}"
export O3K_P12_7_DISCOVERY_URL="${discovery}"
export O3K_P12_7_ALICE_TOKEN="${alice_token}"
export O3K_P12_7_BOB_TOKEN="${bob_token}"
export O3K_P12_7_OPERATOR_TOKEN="${operator_token}"
export O3K_P12_7_UNBOUND_TOKEN="${unbound_token}"
export O3K_P12_7_ALICE_SUBJECT="${alice_subject}"
export O3K_P12_7_BOB_SUBJECT="${bob_subject}"
export O3K_P12_7_OPERATOR_SUBJECT="${operator_subject}"
export O3K_P12_7_BOOTSTRAP_SECRET="${o3k_seed_password}"

cargo test --locked -p o3kd --test p12_iam_7_real_oidc --all-features -- \
  --ignored --nocapture

echo "P12-IAM.7 real federation evidence: PASS"
