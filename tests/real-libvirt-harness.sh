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
case "$*" in
  token\ issue*) exit 0;;
  image\ create*) echo image-id;;
  network\ create*) echo network-id;;
  subnet\ create*) echo subnet-id;;
  flavor\ create*) echo flavor-id;;
  server\ create*) echo server-id;;
  server\ show*) echo '{}';;
  server\ list*) echo '[]';;
  console\ log\ show*) echo 'boot output';;
  *) exit 0;;
esac
SH
chmod +x "${MOCK_BIN}"/*

export PATH="${MOCK_BIN}:${PATH}"
export O3K_TESTLAB_ARTIFACT_DIR="${ARTIFACT_DIR}"
export O3K_TESTLAB_PROFILE=libvirt
export OS_PASSWORD=test-password

bash "${ROOT_DIR}/tests/testlab-libvirt.sh"
python3 - "${ARTIFACT_DIR}/libvirt-result.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1], encoding="utf-8"))
assert result["artifact_type"] == "openstack-cli-e2e"
assert result["status"] == "passed"
assert result["public_api_only"] is True
assert result["cleanup"]["status"] == "passed"
assert set(result["lifecycle"]) == {"create", "show", "list", "stop", "start", "reboot", "console", "delete"}
assert all(result["lifecycle"].values())
PY

echo "real-libvirt harness contract test passed"
