#!/usr/bin/env bash
set -euo pipefail
command -v curl >/dev/null
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
araf_root="${ARAF_ROOT:-/root/araf}"
kc_port="${O3K_P12_7_KEYCLOAK_PORT:?}"; issuer="${O3K_P12_7_ISSUER:?}"; discovery="${O3K_P12_7_DISCOVERY_URL:?}"
db="${O3K_P12_7_SQLITE_PATH:?}"; workdir="${O3K_P12_7_WORKDIR:?}"
o3k_port="${P12_8_O3K_PORT:-18180}"; araf_port="${P12_8_ARAF_PORT:-18181}"
redirect_uri="http://127.0.0.1:${araf_port}/api/v1/auth/callback"; secret=p12-8-araf-client-secret
o3k_pid=""; araf_pid=""
cleanup() {
  [[ -n "${araf_pid}" ]] && kill "${araf_pid}" 2>/dev/null || true
  [[ -n "${o3k_pid}" ]] && kill "${o3k_pid}" 2>/dev/null || true
}
trap cleanup EXIT
sqlite3 "${db}" 'PRAGMA wal_checkpoint(TRUNCATE);'
cp "${db}" "$(dirname "${db}")/o3k.sqlite"
admin_token="$(curl -fsS -X POST "http://127.0.0.1:${kc_port}/realms/master/protocol/openid-connect/token" -d grant_type=password -d client_id=admin-cli -d username=p12-7-admin -d password="${O3K_P12_7_KEYCLOAK_ADMIN_PASSWORD}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')"
client_id="$(curl -fsS -H "Authorization: Bearer ${admin_token}" "http://127.0.0.1:${kc_port}/admin/realms/o3k-p12-7/clients?clientId=o3k-test" | python3 -c 'import json,sys; print(json.load(sys.stdin)[0]["id"])')"
python3 -c 'import json,sys,urllib.request; cid,port,tok,redirect,secret=sys.argv[1:]; p=json.dumps({"clientId":"o3k-test","enabled":True,"publicClient":False,"clientAuthenticatorType":"client-secret","secret":secret,"standardFlowEnabled":True,"directAccessGrantsEnabled":True,"redirectUris":[redirect],"protocol":"openid-connect","protocolMappers":[{"name":"o3k-audience","protocol":"openid-connect","protocolMapper":"oidc-audience-mapper","config":{"included.client.audience":"o3k","id.token.claim":"false","access.token.claim":"true"}}]}).encode(); r=urllib.request.Request(f"http://127.0.0.1:{port}/admin/realms/o3k-p12-7/clients/{cid}",data=p,method="PUT",headers={"Authorization":f"Bearer {tok}","Content-Type":"application/json"}); urllib.request.urlopen(r).read()' "${client_id}" "${kc_port}" "${admin_token}" "${redirect_uri}" "${secret}"
direct_token="$(curl -fsS -X POST "http://127.0.0.1:${kc_port}/realms/o3k-p12-7/protocol/openid-connect/token" -d grant_type=password -d client_id=o3k-test -d client_secret="${secret}" -d username=alice -d password="${P12_8_ALICE_PASSWORD}" | python3 -c 'import json,sys; print(json.load(sys.stdin)["access_token"])')"
echo "seeded Alice binding present: ${O3K_P12_7_ALICE_SUBJECT:+yes}" >&2
(
  cd "${root}"; O3K_LISTEN_ADDR="127.0.0.1:${o3k_port}" O3K_DATA_DIR="$(dirname "${db}")" O3K_PROVIDER=fake O3K_BOOTSTRAP_PASSWORD="${O3K_P12_7_BOOTSTRAP_SECRET}" O3K_TOKEN_SIGNING_KEY=p12-8-live-process-signing-key-at-least-32-bytes O3K_OIDC_TRUST_ID=p12-7-keycloak O3K_OIDC_ISSUER="${issuer}" O3K_OIDC_AUDIENCE=o3k O3K_OIDC_DISCOVERY_URL="${discovery}" O3K_OIDC_ALLOW_INSECURE_LOCAL=true cargo run --quiet -p o3kd
) >"${workdir}/p12-8-o3kd.log" 2>&1 & o3k_pid=$!
for _ in $(seq 1 60); do curl -fsS "http://127.0.0.1:${o3k_port}/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "http://127.0.0.1:${o3k_port}/healthz" >/dev/null
echo "sqlite bindings: $(sqlite3 "$(dirname "${db}")/o3k.sqlite" 'select count(*) from federated_bindings;')" >&2
sqlite3 "$(dirname "${db}")/o3k.sqlite" 'select trusted_issuer_id,issuer,subject,principal_id from federated_bindings order by id;' >&2
scope_probe="$(curl -sS -w '\n%{http_code}' -H 'content-type: application/json' -X POST "http://127.0.0.1:${o3k_port}/o3k/v1/identity/scopes" -d "{\"federated\":{\"access_token\":\"${direct_token}\"}}")"
echo "direct O3K scope probe: ${scope_probe}" >&2
(
  cd "${araf_root}/backend"; ARAF_UPSTREAM_ADAPTER=o3k ARAF_TENANT_OIDC_CLIENT_ID=o3k-test ARAF_TENANT_OIDC_CLIENT_SECRET="${secret}" ARAF_TENANT_OIDC_ISSUER_URL="${issuer}" ARAF_TENANT_OIDC_REDIRECT_URI="${redirect_uri}" ARAF_TENANT_OIDC_AUTHORIZATION_URL="http://127.0.0.1:${kc_port}/realms/o3k-p12-7/protocol/openid-connect/auth" ARAF_TENANT_OIDC_USERINFO_URL="http://127.0.0.1:${kc_port}/realms/o3k-p12-7/protocol/openid-connect/userinfo" O3K_URL="http://127.0.0.1:${o3k_port}" ARAF_TENANT_BFF_PORT="${araf_port}" cargo run --quiet -p tenant-bff
) >"${workdir}/p12-8-araf.log" 2>&1 & araf_pid=$!
for _ in $(seq 1 60); do curl -fsS "http://127.0.0.1:${araf_port}/healthz" >/dev/null 2>&1 && break; sleep 1; done
curl -fsS "http://127.0.0.1:${araf_port}/healthz" >/dev/null
jar="${workdir}/p12-8-browser.cookies"; curl -fsS -D "${workdir}/login.headers" -o /dev/null "http://127.0.0.1:${araf_port}/api/v1/auth/login"
auth_url="$(sed -n 's/^Location: //Ip' "${workdir}/login.headers" | tr -d '\r' | head -1)"; curl -fsS -c "${jar}" -b "${jar}" "${auth_url}" -o "${workdir}/auth.html"
form_action="$(python3 - "${workdir}/auth.html" "${auth_url}" <<'PY'
import re, sys
from urllib.parse import urljoin
html = open(sys.argv[1], encoding="utf-8").read()
match = re.search(r'<form[^>]+action=["\x27]([^"\x27]+)', html, re.IGNORECASE)
if not match:
    raise SystemExit("Keycloak login form action missing")
print(urljoin(sys.argv[2], match.group(1).replace("&amp;", "&")))
PY
)"
login_status="$(curl -sS -D "${workdir}/keycloak.headers" -o "${workdir}/keycloak.body" -w '%{http_code}' -c "${jar}" -b "${jar}" -X POST "${form_action}" -H 'Content-Type: application/x-www-form-urlencoded' --data-urlencode username=alice --data-urlencode "password=${P12_8_ALICE_PASSWORD}" --data-urlencode credentialId= --max-redirs 0 || true)"
callback_location="$(sed -n 's/^Location: //Ip' "${workdir}/keycloak.headers" | tr -d '\r' | head -1)"
echo "Keycloak login status: ${login_status}" >&2
test -n "${callback_location}"
callback_status="$(curl -sS -D "${workdir}/callback.headers" -o "${workdir}/callback.body" -w '%{http_code}' -c "${jar}" -b "${jar}" "${callback_location}")"
echo "Araf callback status: ${callback_status}" >&2
case "${callback_status}" in
  302|303) ;;
  *) exit 1 ;;
esac
echo 'P12.8 step: callback accepted' >&2
session_status="$(curl -fsS -D "${workdir}/session.headers" -o "${workdir}/session.body" -w '%{http_code}' -c "${jar}" -b "${jar}" "http://127.0.0.1:${araf_port}/api/v1/auth/session")"
test "${session_status}" = 200
session_cookie="$(sed -n 's/^Set-Cookie: araf_tenant_session=\([^;]*\).*/\1/Ip' "${workdir}/callback.headers" | head -1)"
csrf_cookie="$(sed -n 's/^Set-Cookie: araf_csrf=\([^;]*\).*/\1/Ip' "${workdir}/callback.headers" | head -1)"
echo "Araf cookie capture: session=$([[ -n "${session_cookie}" ]] && echo yes || echo no) csrf=$([[ -n "${csrf_cookie}" ]] && echo yes || echo no)" >&2
test -n "${session_cookie}"; test -n "${csrf_cookie}"
cookie="araf_tenant_session=${session_cookie}; araf_csrf=${csrf_cookie}"
echo 'P12.8 step: opaque cookies captured' >&2
echo "$(curl -fsS -H "cookie: ${cookie}" "http://127.0.0.1:${araf_port}/api/v1/auth/session")" | grep -q '"authenticated":true'
echo 'P12.8 step: session authenticated' >&2
scopes_response="$(curl -sS -w '\n%{http_code}' -H "cookie: ${cookie}" "http://127.0.0.1:${araf_port}/api/v1/auth/scopes")"
echo "Araf scope response: ${scopes_response}" >&2
echo "${scopes_response}" | grep -q 'project-a'
curl -fsS -H "cookie: ${cookie}" -H "x-csrf-token: ${csrf_cookie}" -H 'content-type: application/json' -X POST "http://127.0.0.1:${araf_port}/api/v1/auth/scope" -d '{"project_id":"project-a"}' >/dev/null
curl -fsS -H "cookie: ${cookie}" "http://127.0.0.1:${araf_port}/api/v1/context" >/dev/null
curl -fsS -H "cookie: ${cookie}" "http://127.0.0.1:${araf_port}/api/v1/resources/compute.server" >/dev/null
if grep -Eiq 'access_token|refresh_token|id_token' "${jar}" "${workdir}/session.headers"; then exit 1; fi
curl -fsS -H "cookie: ${cookie}" -H "x-csrf-token: ${csrf_cookie}" -X POST "http://127.0.0.1:${araf_port}/api/v1/auth/logout" >/dev/null
echo "P12-IAM.8 real Araf process evidence: PASS"
