#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-cli-cleanup.XXXXXX")"
ARTIFACT_DIR="${WORK_DIR}/artifacts"
MOCK_BIN="${WORK_DIR}/bin"
mkdir -p "${ARTIFACT_DIR}" "${MOCK_BIN}"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

cat >"${MOCK_BIN}/openstack" <<'SH'
#!/usr/bin/env bash
set -Eeuo pipefail
printf '%s\n' "$*" >>"${O3K_MOCK_LOG}"
case "$*" in
  token\ issue*) exit 0;;
  image\ create*) echo image-id;;
  network\ create*) echo network-id;;
  subnet\ create*) echo subnet-id;;
  flavor\ create*) echo flavor-id;;
  server\ create*) echo server-id;;
  server\ show*) exit 1;;
  *) exit 0;;
esac
SH
cat >"${MOCK_BIN}/curl" <<'SH'
#!/usr/bin/env bash
exit 0
SH
chmod +x "${MOCK_BIN}/openstack" "${MOCK_BIN}/curl"

export PATH="${MOCK_BIN}:${PATH}"
export O3K_MOCK_LOG="${WORK_DIR}/commands.log"
export O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_TESTLAB_PROFILE=libvirt OS_PASSWORD=test-password
if bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"; then
  echo "CLI harness unexpectedly passed" >&2
  exit 1
fi

python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "failed"
assert result["cleanup"]["status"] == "passed"
PY
for resource in "server delete --wait server-id" "flavor delete flavor-id" \
                "subnet delete subnet-id" "network delete network-id" "image delete image-id"; do
  grep -Fq "${resource}" "${O3K_MOCK_LOG}"
done

echo "OpenStack CLI cleanup test passed"
