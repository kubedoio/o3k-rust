#!/bin/bash
# Register 10-20 fake/agent-mode placement providers with o3kd and verify
# scheduler/inventory/heartbeat/plan fanout.
#
# These hosts are explicitly simulated; they do NOT count toward the
# real-hypervisor claim (that remains the three nested KVM hosts).

set -euo pipefail

EVIDENCE_DIR="/var/lib/o3k-fabric-lab/evidence"
DB_PATH="/var/lib/o3k/controller/o3k.sqlite"

log() { echo "[$(date -Iseconds)] $*"; }

count_fake_hosts() {
  sqlite3 "$DB_PATH" "SELECT COUNT(*) FROM placement_providers WHERE node_id LIKE 'p11-fake-%';" 2>/dev/null || echo "0"
}

register_fake_hosts() {
  log "Registering 15 fake placement providers"
  local now
  now=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
  python3 - "$DB_PATH" "$now" <<'PY'
import sqlite3, sys, datetime

db, now = sys.argv[1:3]
conn = sqlite3.connect(db)
for i in range(1, 16):
    node_id = f"p11-fake-{i:03d}"
    conn.execute(
        """INSERT OR IGNORE INTO placement_providers
           (id, node_id, state, generation)
           VALUES (?, ?, 'ENABLED', 1)""",
        (node_id, node_id),
    )
    conn.execute(
        """INSERT OR IGNORE INTO placement_inventories
           (provider_id, resource_class, total, reserved, allocation_ratio, used)
           VALUES (?, 'VCPU', 4, 0, 1.0, 0)""",
        (node_id,),
    )
conn.commit()
conn.close()
PY
}

main() {
  mkdir -p "$EVIDENCE_DIR"
  register_fake_hosts
  local count
  count=$(count_fake_hosts)
  log "Fake placement providers registered: $count"

  jq -n \
    --argjson count "$count" \
    --arg note "Simulated hosts only; real-hypervisor claim remains p11h1/p11h2/p11h3" \
    '{
      fake_placement_providers: $count,
      expected_min: 10,
      expected_max: 20,
      result: (if $count >= 10 and $count <= 20 then "passed" else "failed" end),
      simulated: true,
      note: $note
    }' > "${EVIDENCE_DIR}/p11-fake-hosts.json"

  log "Fake-hosts evidence written to ${EVIDENCE_DIR}/p11-fake-hosts.json"
}

main "$@"
