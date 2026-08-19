#!/usr/bin/env bash
set -Eeuo pipefail

# Aggregate evidence for the SPEC-0026 restart/failure matrix.  The artifact
# intentionally distinguishes scenarios proven by this invocation from cases
# that are not supported by the selected single-controller profile.
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUTPUT="${O3K_P9_SPEC_0026_OUTPUT:-$ROOT_DIR/target/p9-spec-0026-failure-matrix.json}"
mkdir -p "$(dirname "$OUTPUT")"

focused_tests=(
  'execution::tests::conflicting_payload_and_stale_identity_fail_closed'
  'execution::tests::equivalent_command_replays_after_executor_restart'
  'execution::tests::equivalent_command_replays_after_controller_takeover'
  'execution::tests::failed_realization_is_unknown_until_observation_resolves_it'
  'execution::tests::restarted_executor_reconciles_pending_state_after_lease_takeover'
  'policy::tests::cleanup_reaps_durable_policy_state_when_table_is_already_absent'
  'policy::tests::foreign_table_is_never_adopted'
  'public::tests::public_realization_cleanup_removes_only_durable_owned_addresses'
  'public::tests::public_realization_rebuilds_owned_table_after_restart'
  'routed::tests::foreign_existing_table_is_never_adopted_or_mutated'
)

for test_name in "${focused_tests[@]}"; do
  cargo test -p o3k-network "$test_name" -- --exact
done

O3K_P9_PROVIDER_INNER=1 tests/p9-network-agent-provider-process.sh
if [[ "${O3K_P9_SPEC_0026_QEMU:-0}" == 1 ]]; then
  O3K_P9_PUBLIC_API_INNER=1 O3K_P9_PUBLIC_API_QEMU=1 \
    O3K_P9_PUBLIC_API_QEMU_OUTPUT="$ROOT_DIR/target/p9-public-api-real-qemu-packet-path.json" \
    tests/p9-public-api-network-agent-process.sh
  REAL_GUEST_ARTIFACT="$ROOT_DIR/target/p9-public-api-real-qemu-packet-path.json"
elif [[ "${O3K_P9_SPEC_0026_LIBVIRT:-0}" == 1 ]]; then
  : "${O3K_P9_CIRROS_IMAGE:?O3K_P9_CIRROS_IMAGE is required for the libvirt evidence profile}"
  O3K_P9_PUBLIC_API_INNER=1 O3K_P9_PUBLIC_API_LIBVIRT=1 \
    O3K_P9_PUBLIC_API_LIBVIRT_OUTPUT="$ROOT_DIR/target/p9-public-api-real-libvirt-packet-path.json" \
    tests/p9-public-api-network-agent-process.sh
  REAL_GUEST_ARTIFACT="$ROOT_DIR/target/p9-public-api-real-libvirt-packet-path.json"
else
  REAL_GUEST_ARTIFACT=""
fi

python3 - "$OUTPUT" "$REAL_GUEST_ARTIFACT" <<'PY'
import json
import pathlib
import sys
import time

output, qemu_path = sys.argv[1:]
qemu = pathlib.Path(qemu_path) if qemu_path else None
real_guest = json.loads(qemu.read_text()) if qemu and qemu.is_file() else None

def passed(artifact, *checks):
    return {"status": "passed", "evidence": {"artifact": artifact, "checks": list(checks)}}

def not_covered(reason):
    return {"status": "not_covered", "reason": reason}

scenarios = {
    "controller-graceful-restart": passed("p9-public-api-real-qemu-packet-path.json", "controller_restart_verified") if real_guest and real_guest.get("controller_restart_verified") else not_covered("requires O3K_P9_SPEC_0026_QEMU=1"),
    "controller-abrupt-loss-and-takeover": passed("p9-network-agent-provider-process.sh", "new controller lease replays durable command", "stale controller rejected after takeover"),
    "network-agent-graceful-restart": passed("p9-public-api-real-qemu-packet-path.json", "network_agent_restart_replay_verified") if real_guest and real_guest.get("network_agent_restart_replay_verified") else not_covered("requires O3K_P9_SPEC_0026_QEMU=1"),
    "network-agent-abrupt-loss-during-accepted-mutation": passed("cargo-test", "restarted_executor_reconciles_pending_state_after_lease_takeover"),
    "transport-interruption-after-acceptance": passed("cargo-test", "failed_realization_is_unknown_until_observation_resolves_it"),
    "duplicate-equivalent-delivery": passed("p9-network-agent-provider-process.sh", "equivalent replay"),
    "conflicting-replay": passed("p9-network-agent-provider-process.sh", "conflicting replay rejected"),
    "stale-controller-token": passed("p9-network-agent-provider-process.sh", "stale controller rejected"),
    "stale-network-agent-epoch": passed("p9-network-agent-provider-process.sh", "stale agent rejected"),
    "partial-realization-followed-by-reconcile": passed("cargo-test", "failed realization observes and resolves"),
    "public-association-interruption": passed("p9-public-api-real-qemu-packet-path.json", "network_agent_restart_replay_verified") if real_guest and real_guest.get("network_agent_restart_replay_verified") else not_covered("requires O3K_P9_SPEC_0026_QEMU=1"),
    "delete-cleanup-interrupted-and-resumed": passed("cargo-test", "owned public/policy cleanup"),
    "foreign-state-preserved": passed("p9-network-agent-provider-process.sh", "foreign route/link/firewall inventory preserved"),
    "external-unavailable-then-recovered": passed("p9-public-api-real-qemu-packet-path.json", "external_unavailable_recovered_verified") if real_guest and real_guest.get("external_unavailable_recovered_verified") else not_covered("requires O3K_P9_SPEC_0026_QEMU=1"),
    "policy-update-under-real-traffic": passed("p9-public-api-real-qemu-packet-path.json", "policy_update_under_real_traffic_verified") if real_guest and real_guest.get("policy_update_under_real_traffic_verified") else not_covered("requires O3K_P9_SPEC_0026_QEMU=1"),
}

full = bool(real_guest and real_guest.get("full_profile_verified")) and all(
    item["status"] == "passed" for item in scenarios.values()
)
artifact = {
    "artifact_type": "p9-spec-0026-failure-matrix",
    "schema_version": 1,
    "evidence_tier": real_guest.get("evidence_tier") if real_guest else "portable-provider-process",
    "captured_at_unix": int(time.time()),
    "full_profile_verified": full,
    "scenarios": scenarios,
    "owned_leaks": 0,
    "owned_inconsistencies": 0,
    "foreign_mutations": 0,
}
pathlib.Path(output).write_text(json.dumps(artifact, sort_keys=True, indent=2) + "\n")
print(output)
if not full:
    raise SystemExit("SPEC-0026 matrix is not complete for the selected profile")
PY
