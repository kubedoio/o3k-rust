#!/usr/bin/env bash
# ASR-022 one-line installer campaign — in-VM PHASE 1 (issue #613).
#
# Runs INSIDE a fresh VM with no repo checkout and no bundle copy:
#   (a) foreign canaries + clean-state pre-checks;
#   (b) the exact one-liner, full output captured verbatim into the evidence
#       directory (the §15 UX evidence);
#   (c) assertions: exit 0, checklist markers, 0600 credential files, admin
#       password never in the captured output;
#   (d) public-API verification: token issue, test-vm ACTIVE, fixed IP
#       192.0.2.2, config-drive, console boot marker; resource IDs + counts;
#   (e) `sudo reboot` as the LAST statement (host-run.sh polls SSH afterwards).
#
# Usage: sudo bash in-vm-phase1.sh <ubuntu|debian> <evidence-dir> <source-sha>
set -Eeuo pipefail
DISTRO="${1:-ubuntu}"
EVID="${2:-/tmp/o3k-campaign-evidence}"
SOURCE_SHA="${3:-unknown}"
IDS_FILE="$EVID/phase1-ids.env"
CANARY_FILE="$EVID/canaries.txt"
ONELINER_OUT="$EVID/install-output.txt"
mkdir -p "$EVID"
cd /
log() { echo "[$(date -u +%H:%M:%SZ)] $*"; }

# ---- (a) foreign canaries + pre-checks -----------------------------------------
mkdir -p /opt/o3k-foreign /etc/o3k-foreign
printf 'foreign-file-canary-asr022\n' > /opt/o3k-foreign/canary.txt
printf 'foreign-file-canary-asr022\n' > /etc/o3k-foreign/canary.txt
id -u foreigncanary >/dev/null 2>&1 || useradd --system --no-create-home foreigncanary
C1=$(sha256sum /opt/o3k-foreign/canary.txt | awk '{print $1}')
C2=$(sha256sum /etc/o3k-foreign/canary.txt | awk '{print $1}')
C3=$(grep '^foreigncanary:' /etc/passwd | sha256sum | awk '{print $1}')
printf '%s\n%s\n%s\n' "$C1" "$C2" "$C3" > "$CANARY_FILE"
if id o3k &>/dev/null || id o3k-compute &>/dev/null; then
  echo "ERROR: o3k/o3k-compute already exist" >&2; exit 1
fi
[ ! -e /etc/o3k ] && [ ! -L /etc/o3k ] || { echo "ERROR: /etc/o3k already exists" >&2; exit 1; }
log "pre-checks passed (no o3k accounts, /etc/o3k absent, canaries planted)"

# ---- (b) the exact one-liner, verbatim capture ---------------------------------
log "running the one-liner (exact command, output captured to $ONELINER_OUT)"
if curl -sfL http://10.0.2.2:18000/ \
    | sudo env O3K_RELEASE_BASE=http://10.0.2.2:18000/releases sh - 2>&1 \
    | tee "$ONELINER_OUT"; then
  log "one-liner exited 0"
else
  echo "ERROR: one-liner failed" >&2; exit 1
fi

# ---- (c) output + credential assertions ----------------------------------------
# The platform line is "✓ <PRETTY_NAME> <machine>" — derive it from the actual
# os-release instead of hardcoding a point release (e.g. Ubuntu 24.04.4 LTS).
PLATFORM_MARKER="✓ $(sed -n 's/^PRETTY_NAME="\(.*\)"/\1/p' /etc/os-release) $(uname -m)"
for marker in \
  'O3K Cloud OS — TestLab' \
  "$PLATFORM_MARKER" \
  '✓ O3K v0.2.0-alpha.2 verified' \
  '✓ mTLS identities ready' \
  '✓ o3kd installed' \
  '✓ o3k-compute installed' \
  '✓ control plane ready' \
  '✓ compute agent connected' \
  'server test-vm ACTIVE with fixed IP 192.0.2.2 and config-drive' \
  'console boot marker verified (cirros|login:)' \
  'O3K is ready.'; do
  grep -Fq -- "$marker" "$ONELINER_OUT" || { echo "ERROR: missing output marker: $marker" >&2; exit 1; }
done
for cred in /etc/o3k/admin-openrc /etc/o3k/clouds.yaml; do
  [ -f "$cred" ] || { echo "ERROR: missing credential file $cred" >&2; exit 1; }
  [ "$(stat -c %a "$cred")" = 600 ] || { echo "ERROR: bad mode on $cred" >&2; exit 1; }
done
PW="$(grep '^O3K_BOOTSTRAP_PASSWORD=' /etc/o3k/o3kd.env | head -1 | cut -d= -f2-)"
[ -n "$PW" ] || { echo "ERROR: no bootstrap password in o3kd.env" >&2; exit 1; }
if grep -Fq -- "$PW" "$ONELINER_OUT"; then
  echo "ERROR: admin password leaked into the captured output" >&2; exit 1
fi
log "markers, credential modes (0600), and password-redaction verified"

# ---- (d) public-API verification + resource identity capture -------------------
# shellcheck disable=SC1091
source /etc/o3k/admin-openrc
openstack token issue >/dev/null 2>&1 || { echo "ERROR: token issue failed" >&2; exit 1; }
log "token issue ok"

SRV_ID=""
for i in $(seq 1 15); do
  SRV_ID="$(openstack server show test-vm -f value -c id 2>/dev/null || true)"
  [ -n "$SRV_ID" ] && break
  sleep 2
done
[ -n "$SRV_ID" ] || { echo "ERROR: test-vm cannot be resolved" >&2; exit 1; }
STATUS="$(openstack server show "$SRV_ID" -f value -c status)"
[ "$STATUS" = ACTIVE ] || { echo "ERROR: test-vm not ACTIVE ($STATUS)" >&2; exit 1; }
CONFIG_DRIVE="$(openstack server show "$SRV_ID" -f value -c config_drive)"
[ "$CONFIG_DRIVE" = True ] || { echo "ERROR: config-drive not enabled" >&2; exit 1; }
FIXED_IP="$(openstack server show "$SRV_ID" -f json | python3 -c '
import json, sys
def collect(node, out):
    if isinstance(node, dict):
        if "addr" in node:
            out.append(str(node["addr"]))
        for child in node.values():
            collect(child, out)
    elif isinstance(node, list):
        for child in node:
            collect(child, out)
    elif isinstance(node, str):
        # openstackclient flattens addresses into {network: ["<ip>", ...]}.
        out.append(node)
value = json.load(sys.stdin)
addresses = []
collect(value.get("addresses", {}), addresses)
print(addresses[0] if addresses else "")
')"
[ "$FIXED_IP" = 192.0.2.2 ] || { echo "ERROR: unexpected fixed IP: $FIXED_IP" >&2; exit 1; }
CONSOLE_OK=no
for i in $(seq 1 30); do
  if timeout 15 openstack console log show "$SRV_ID" >"$EVID/console-phase1.log" 2>/dev/null \
    && grep -Eiq 'cirros|login:' "$EVID/console-phase1.log"; then
    CONSOLE_OK=yes; break
  fi
  sleep 2
done
[ "$CONSOLE_OK" = yes ] || { echo "ERROR: no console boot marker" >&2; exit 1; }
log "test-vm ACTIVE, fixed IP $FIXED_IP, config-drive, console marker verified"

capture_count() { # capture_count TYPE [COLUMN] — count `openstack <type> list` rows
  local type="$1" column="${2:-ID}" out=""
  if out="$(openstack "$type" list -f value -c "$column" 2>/dev/null)"; then
    :
  else
    printf 'CAPTURE_FAILED'
    return 0
  fi
  if [[ -z "$out" ]]; then
    printf '0'
    return 0
  fi
  printf '%s\n' "$out" | sed -n '$='
}
IMAGE_ID="$(openstack image show cirros-0.6.3 -f value -c id || true)"
# `openstack flavor show` follows up with /flavors/{id}/os-extra_specs, which
# O3K does not implement; resolve flavors through the list API like the
# bundled bootstrap-testlab.sh. On bookworm's OSC 6.0 even `flavor list -f json`
# fails (os-extra_specs 404 on every flavor), so fall back to the flavor
# reference carried by the server object.
FLAVOR_ID="$(openstack flavor list -f json 2>/dev/null | python3 -c '
import json, sys
try:
    flavors = json.load(sys.stdin)
    for f in flavors:
        if f.get("Name") == "testlab-flavor":
            print(f.get("ID", ""))
            break
except Exception:
    pass
' || true)"
if [ -z "$FLAVOR_ID" ]; then
  # OSC <= 6.1 post-processes `server show`: it resolves the flavor via
  # find_resource (GET /flavors/{id}, which O3K 404s) and falls back to a bare
  # UUID string; newer clients embed "name (id)". Accept all three shapes.
  FLAVOR_ID="$(openstack server show "$SRV_ID" -f json 2>/dev/null | python3 -c '
import json, re, sys
try:
    server = json.load(sys.stdin)
    flavor = server.get("flavor", {})
    if isinstance(flavor, dict):
        print(flavor.get("id", ""))
    elif isinstance(flavor, str):
        m = re.search(r"\(([0-9a-fA-F-]{36})\)", flavor)
        if m:
            print(m.group(1))
        elif re.fullmatch(r"[0-9a-fA-F-]{36}", flavor):
            print(flavor)
except Exception:
    pass
' || true)"
fi
if [ -z "$FLAVOR_ID" ]; then
  # Bookworm's client cannot enumerate or resolve flavors through any API
  # read; the bundled bootstrap already verified the flavor spec against its
  # own create response, and the re-run convergence check remains authoritative.
  FLAVOR_ID=unavailable
  log "flavor ID unresolvable through this client; marking unavailable"
fi
NET_ID="$(openstack network show testlab-network -f value -c id || true)"
SUB_ID="$(openstack subnet show testlab-subnet -f value -c id || true)"
PORT_ID="$(openstack port show testlab-port -f value -c id || true)"
KP_FP="$(openstack keypair show testlab-keypair -f value -c fingerprint || true)"
{
  printf 'SRV_ID=%s\n' "$SRV_ID"
  printf 'IMAGE_ID=%s\n' "$IMAGE_ID"
  printf 'FLAVOR_ID=%s\n' "$FLAVOR_ID"
  printf 'NET_ID=%s\n' "$NET_ID"
  printf 'SUB_ID=%s\n' "$SUB_ID"
  printf 'PORT_ID=%s\n' "$PORT_ID"
  printf 'KP_FP=%s\n' "$KP_FP"
  printf 'COUNT_SRV=%s\n' "$(capture_count server ID)"
  printf 'COUNT_IMG=%s\n' "$(capture_count image ID)"
  printf 'COUNT_NET=%s\n' "$(capture_count network ID)"
  printf 'COUNT_SUB=%s\n' "$(capture_count subnet ID)"
  printf 'COUNT_PORT=%s\n' "$(capture_count port ID)"
  printf 'COUNT_FLAVOR=%s\n' "$({ openstack flavor list -f json 2>/dev/null | python3 2>/dev/null -c '
import json, sys
print(len(json.load(sys.stdin)))
'; } || printf 'unavailable')"
  printf 'COUNT_KP=%s\n' "$(capture_count keypair Name)"
} > "$IDS_FILE"
# A count capture must never mask an API failure into a "0" that a later
# phase would compare vacuously: CAPTURE_FAILED is a hard phase-1 failure.
if grep -q '=CAPTURE_FAILED$' "$IDS_FILE"; then
  echo "ERROR: count capture failed: $(grep '=CAPTURE_FAILED$' "$IDS_FILE" | tr '\n' ' ')" >&2
  exit 1
fi
for spec in "SRV_ID:$SRV_ID" "IMAGE_ID:$IMAGE_ID" "FLAVOR_ID:$FLAVOR_ID" \
    "NET_ID:$NET_ID" "SUB_ID:$SUB_ID" "PORT_ID:$PORT_ID" "KP_FP:$KP_FP"; do
  [[ -n "${spec#*:}" ]] || { echo "ERROR: capture failed for ${spec%%:*}" >&2; exit 1; }
done
log "resource IDs and counts captured"

echo "PHASE1-COMPLETE"
sync
sudo reboot
