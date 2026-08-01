#!/usr/bin/env bash
set -Eeuo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/o3k-real-libvirt-harness.XXXXXX")"
ARTIFACT_DIR="${WORK_DIR}/artifacts"
MOCK_BIN="${WORK_DIR}/bin"
mkdir -p "${ARTIFACT_DIR}" "${MOCK_BIN}"
trap 'rm -rf -- "${WORK_DIR}"' EXIT

cat >"${MOCK_BIN}/virsh" <<'SH'
#!/usr/bin/env bash
set -Eeuo pipefail
if [[ "$*" == "-c qemu:///system uri" ]]; then
  echo qemu:///system
fi
SH
cat >"${MOCK_BIN}/ip" <<'SH'
#!/usr/bin/env bash
exit 0
SH
cat >"${MOCK_BIN}/qemu-img" <<'SH'
#!/usr/bin/env bash
exit 0
SH
cat >"${MOCK_BIN}/curl" <<'SH'
#!/usr/bin/env bash
exit 0
SH
cat >"${MOCK_BIN}/openstack" <<'SH'
#!/usr/bin/env bash
set -Eeuo pipefail
state_file="${O3K_MOCK_STATE:?}"
resource_state="${O3K_MOCK_RESOURCE_STATE:?}"
case "$*" in
  token\ issue*) exit 0;;
  image\ create*) echo image-id; : >"${resource_state}/image-id";;
  keypair\ create*) echo keypair-id; : >"${resource_state}/keypair-id";;
  network\ create*) echo network-id; : >"${resource_state}/network-id";;
  subnet\ create*) echo subnet-id; : >"${resource_state}/subnet-id";;
  flavor\ create*) echo flavor-id; : >"${resource_state}/flavor-id";;
  image\ show*|keypair\ show*|network\ show*|subnet\ show*|flavor\ show*)
    resource="${1}"; id="${3}"
    if [[ -e "${resource_state}/${id}" ]]; then
      printf '{"id":"%s"}\n' "${id}"
    else
      printf 'No %s with ID was found\n' "${resource}" >&2
      exit 1
    fi
    ;;
  image\ delete*|keypair\ delete*|network\ delete*|subnet\ delete*|flavor\ delete*)
    rm -f -- "${resource_state}/${3}"
    ;;
  server\ create*) : >"${state_file}"; echo server-id;;
  server\ show*)
    if [[ -e "${state_file}" ]]; then
      echo '{"id":"server-id","name":"o3k-testlab-server","status":"ACTIVE","config_drive":true,"addresses":{"o3k-testlab-network":[{"addr":"192.0.2.2"}]}}'
    else
      echo 'No server with a name or ID was found' >&2
      exit 1
    fi
    ;;
  server\ list*)
    if [[ -e "${state_file}" ]]; then
      echo '[{"id":"server-id","name":"o3k-testlab-server"}]'
    else
      echo '[]'
    fi
    ;;
  server\ delete*) rm -f -- "${state_file}";;
  console\ log\ show*) echo 'CirrOS boot output\nlogin:';;
  *) exit 0;;
esac
SH
chmod +x "${MOCK_BIN}"/*

export PATH="${MOCK_BIN}:${PATH}"
export O3K_MOCK_STATE="${WORK_DIR}/server-present"
export O3K_MOCK_RESOURCE_STATE="${WORK_DIR}/resources"
mkdir -p "${O3K_MOCK_RESOURCE_STATE}"
export O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_TESTLAB_PROFILE=libvirt
export OS_PASSWORD=test-password
IMAGE_PATH="${WORK_DIR}/cirros.img"
printf 'test image\n' >"${IMAGE_PATH}"
export O3K_TESTLAB_IMAGE_PATH="${IMAGE_PATH}" O3K_TESTLAB_CONSOLE_ATTEMPTS=1

bash "${ROOT_DIR}/tests/testlab-libvirt.sh"
python3 - "${ARTIFACT_DIR}/libvirt-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["artifact_type"] == "openstack-cli-e2e"
assert result["status"] == "passed"
assert result["public_api_only"] is True
assert result["cleanup"]["status"] == "passed"
assert result["acceptance"] == {"status": "ACTIVE", "fixed_ip": "192.0.2.2", "config_drive": True, "console_boot_marker": True}
assert set(result["lifecycle"]) == {"create", "show", "list", "stop", "start", "reboot", "console", "delete"}
assert all(result["lifecycle"].values())
PY

echo "real-libvirt harness contract test passed"
