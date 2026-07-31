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
state_file="${O3K_MOCK_STATE:?}"
mode="${O3K_MOCK_MODE:-normal}"
case "$*" in
  token\ issue*) exit 0;;
  image\ create*) echo image-id;;
  network\ create*) echo network-id;;
  subnet\ create*) echo subnet-id;;
  flavor\ create*) echo flavor-id;;
  server\ create*) : >"${state_file}"; echo server-id;;
  server\ show*)
    if [[ ! -e "${state_file}" ]]; then
      echo 'No server with a name or ID was found' >&2
      exit 1
    fi
    case "${mode}" in
      empty) echo '{}';;
      unrelated) echo '{"id":"unrelated-server"}';;
      *) echo '{"id":"server-id","name":"o3k-testlab-server"}';;
    esac
    ;;
  server\ list*)
    if [[ ! -e "${state_file}" ]]; then
      echo '[]'
    elif [[ "${mode}" == empty-list ]]; then
      echo '[]'
    elif [[ "${mode}" == unrelated ]]; then
      echo '[{"id":"unrelated-server"}]'
    else
      echo '[{"id":"server-id","name":"o3k-testlab-server"}]'
    fi
    ;;
  server\ delete*)
    if [[ "${mode}" != noop-delete ]]; then
      rm -f -- "${state_file}"
    fi
    ;;
  console\ log\ show*) echo 'boot output';;
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
export O3K_MOCK_STATE="${WORK_DIR}/server-present"
export O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_TESTLAB_PROFILE=libvirt OS_PASSWORD=test-password
IMAGE_PATH="${WORK_DIR}/cirros.img"
printf 'test image\n' >"${IMAGE_PATH}"
export O3K_TESTLAB_IMAGE_PATH="${IMAGE_PATH}" O3K_TESTLAB_CONSOLE_ATTEMPTS=1
assert_failed() {
  local mode="$1" cleanup_status="$2"
  rm -f -- "${ARTIFACT_DIR}/openstack-cli-result.json"
  if O3K_MOCK_MODE="${mode}" bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"; then
    echo "CLI harness unexpectedly passed in ${mode} mode" >&2
    exit 1
  fi
  python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" "${cleanup_status}" <<'PY'
import json
import sys

result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "failed"
assert result["cleanup"]["status"] == sys.argv[2]
assert result["resources"]["server_id"] == "server-id"
PY
}

assert_failed empty passed
assert_failed unrelated passed
assert_failed empty-list passed

python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" "${ARTIFACT_DIR}" <<'PY'
import pathlib
import sys
assert not (pathlib.Path(sys.argv[2]) / "console-error.log").exists()
PY
for resource in "server delete --wait server-id" "flavor delete flavor-id" \
                "subnet delete subnet-id" "network delete network-id" "image delete image-id"; do
  grep -Fq "${resource}" "${O3K_MOCK_LOG}"
done

O3K_MOCK_MODE=normal bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"
python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "passed"
assert result["lifecycle"]["list"] is True
assert result["resources"]["server_id"] == "server-id"
PY
grep -Fq "server list --name o3k-testlab-server -f json" "${O3K_MOCK_LOG}"
grep -Fq "image create o3k-testlab-image --file" "${O3K_MOCK_LOG}"
grep -Fq "server create --wait" "${O3K_MOCK_LOG}"
grep -Fq "server stop --wait" "${O3K_MOCK_LOG}"
grep -Fq "server start --wait" "${O3K_MOCK_LOG}"
grep -Fq "server reboot --hard --wait" "${O3K_MOCK_LOG}"

if O3K_MOCK_MODE=noop-delete bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"; then
  echo "CLI harness accepted a no-op server delete" >&2
  exit 1
fi
python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "failed"
assert result["cleanup"]["status"] == "failed"
PY

echo "OpenStack CLI cleanup test passed"
