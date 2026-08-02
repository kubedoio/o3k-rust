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
state_dir="${O3K_MOCK_STATE_DIR:?}"
mode="${O3K_MOCK_MODE:-normal}"
case "$*" in
  token\ issue*) exit 0;;
  image\ create*) : >"${state_dir}/image-image-id"; echo image-id;;
  keypair\ create*) : >"${state_dir}/keypair-keypair-id"; echo keypair-id;;
  network\ create*) : >"${state_dir}/network-network-id"; echo network-id;;
  subnet\ create*) : >"${state_dir}/subnet-subnet-id"; echo subnet-id;;
  port\ create*) : >"${state_dir}/port-port-id"; echo port-id;;
  flavor\ create*) : >"${state_dir}/flavor-flavor-id"; echo flavor-id;;
  image\ show*)
    if [[ -e "${state_dir}/image-image-id" ]]; then echo '{}'; else echo 'No image with a name or ID was found' >&2; exit 1; fi;;
  keypair\ show*)
    if [[ -e "${state_dir}/keypair-keypair-id" ]]; then echo '{}'; else echo 'No keypair with a name or ID was found' >&2; exit 1; fi;;
  network\ show*)
    if [[ -e "${state_dir}/network-network-id" ]]; then echo '{}'; else echo 'No network with a name or ID was found' >&2; exit 1; fi;;
  subnet\ show*)
    if [[ -e "${state_dir}/subnet-subnet-id" ]]; then echo '{}'; else echo 'No subnet with a name or ID was found' >&2; exit 1; fi;;
  port\ show*)
    if [[ -e "${state_dir}/port-port-id" ]]; then echo '{}'; else echo 'No port with a name or ID was found' >&2; exit 1; fi;;
  flavor\ show*)
    if [[ -e "${state_dir}/flavor-flavor-id" ]]; then echo '{}'; else echo 'No flavor with a name or ID was found' >&2; exit 1; fi;;
  server\ create*) : >"${state_file}"; echo server-id;;
  server\ show*)
    if [[ ! -e "${state_file}" ]]; then
      echo 'No server with a name or ID was found' >&2
      exit 1
    fi
    case "${mode}" in
      empty) echo '{}';;
      unrelated) echo '{"id":"unrelated-server"}';;
    *)
      config_drive=true
      [[ "${O3K_TESTLAB_CONFIG_DRIVE:-true}" == false ]] && config_drive=false
      echo "{\"id\":\"server-id\",\"name\":\"o3k-testlab-server\",\"status\":\"ACTIVE\",\"config_drive\":${config_drive},\"addresses\":{\"o3k-testlab-network\":[{\"addr\":\"192.0.2.2\"}]}}";;
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
  image\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/image-image-id";;
  keypair\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/keypair-keypair-id";;
  network\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/network-network-id";;
  subnet\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/subnet-subnet-id";;
  port\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/port-port-id";;
  flavor\ delete*) [[ "${mode}" != noop-dependent-delete ]] && rm -f -- "${state_dir}/flavor-flavor-id";;
  console\ log\ show*) echo 'CirrOS boot output\nlogin:';;
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
export O3K_MOCK_STATE_DIR="${WORK_DIR}/resource-state"
mkdir -p "${O3K_MOCK_STATE_DIR}"
export O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_TESTLAB_PROFILE=libvirt OS_PASSWORD=$'test:password\nwith-special-characters'
export OS_AUTH_URL='https://identity.example.test/v3?scope=a:b'
export OS_USERNAME='user:with:colon' OS_PROJECT_NAME='project:with:colon' OS_REGION_NAME='Region:One'
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
assert result["resources"]["keypair_id"] == "o3k-testlab-keypair"
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
                "port delete port-id" "keypair delete o3k-testlab-keypair" "subnet delete subnet-id" \
                "network delete network-id" "image delete image-id"; do
  grep -Fq "${resource}" "${O3K_MOCK_LOG}"
done

O3K_MOCK_MODE=normal bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"
python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "passed"
assert result["lifecycle"]["list"] is True
assert result["acceptance"] == {"status": "ACTIVE", "fixed_ip": "192.0.2.2", "config_drive": True, "console_boot_marker": True, "restart": {"status": "ACTIVE", "fixed_ip": "192.0.2.2", "config_drive": True}}
assert result["resources"]["server_id"] == "server-id"
assert set(result["cleanup"]["resources"]) == {"image", "keypair", "network", "subnet", "port", "flavor", "server"}
assert all(value == "verified_absent" for value in result["cleanup"]["resources"].values())
PY
grep -Fq "server list --name o3k-testlab-server -f json" "${O3K_MOCK_LOG}"
grep -Fq "image create o3k-testlab-image --file" "${O3K_MOCK_LOG}"
grep -Fq "server create --wait" "${O3K_MOCK_LOG}"
grep -Fq "keypair create --public-key" "${O3K_MOCK_LOG}"
grep -Fq -- "--key-name o3k-testlab-keypair" "${O3K_MOCK_LOG}"
grep -Fq -- "--config-drive true" "${O3K_MOCK_LOG}"
grep -Fq -- "--nic port-id=port-id" "${O3K_MOCK_LOG}"
grep -Fq "server stop --wait" "${O3K_MOCK_LOG}"
grep -Fq "server start --wait" "${O3K_MOCK_LOG}"
grep -Fq "server reboot --hard --wait" "${O3K_MOCK_LOG}"

O3K_TESTLAB_CONFIG_DRIVE=false O3K_MOCK_MODE=normal bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"
python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "passed"
assert result["acceptance"]["config_drive"] is False
assert result["acceptance"]["restart"]["config_drive"] is False
PY
grep -Fq -- "--no-config-drive" "${O3K_MOCK_LOG}"

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

if O3K_MOCK_MODE=noop-dependent-delete bash "${ROOT_DIR}/tests/openstack-cli-libvirt.sh"; then
  echo "CLI harness accepted a no-op dependent-resource delete" >&2
  exit 1
fi
python3 - "${ARTIFACT_DIR}/openstack-cli-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["status"] == "failed"
assert result["cleanup"]["status"] == "failed"
assert result["cleanup"]["resources"]["flavor"] == "not_verified"
PY

echo "OpenStack CLI cleanup test passed"
