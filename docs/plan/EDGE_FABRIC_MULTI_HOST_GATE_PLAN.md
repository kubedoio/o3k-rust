# P11 Real Multi-Host Gate Plan

This document records the implementation plan and runtime assumptions for the
P11 real multi-host gate harness in the `codex/p11-fip-next` branch.

## Normative basis

- ADR-0171 — AddressRealm-encapsulated Edge Fabric v2
- SPEC-0029 — AddressRealm-encapsulated Edge Fabric v2
- `contracts/edge-fabric-realm-overlay.md`
- `docs/P11_REALM_OVERLAY_IMPLEMENTATION_PROMPT.md`

## Gate topology

Three independent nested KVM hosts:

```text
p11h1 = 10.77.0.11
p11h2 = 10.77.0.12
p11h3 = 10.77.0.13
```

Controller `o3kd` runs on `10.77.0.1`.

## Tenant layout

```text
Project A / Realm A: 10.0.0.0/24
  A1 = 10.0.0.10 on p11h1
  A2 = 10.0.0.20 on p11h2

Project B / Realm B: 10.0.0.0/24
  B1 = 10.0.0.10 on p11h3
  B2 = 10.0.0.20 on p11h2
```

## Known limitation

`o3kd` currently supports only a single `O3K_NETWORK_AGENT_ENDPOINT`. The gate
harness therefore drives each host's `o3k-network` agent directly via the
existing `o3k-network-protocol::NetworkAgentClient` (mTLS gRPC). This is
intentional and documented.

## Deliverables

1. `scripts/edge-fabric-gate.sh` — top-level orchestrator.
2. `crates/o3k-network/examples/p11-multi-host-driver.rs` — Rust fabric driver.
3. `scripts/edge-fabric-domain-helper.sh` — tenant VM helper via direct `virsh`.
4. `scripts/edge-fabric-cleanup-inventory.sh` — cleanup + zero-leak inventory.
5. `scripts/edge-fabric-storage-evidence.sh` — LVM locality + RBD readiness evidence.
6. `scripts/edge-fabric-fake-hosts.sh` — fake placement providers for scheduler fanout.

## Resource creation

Projects, networks, subnets and ports with fixed IPs are created directly in
the controller SQLite database because the OpenStack-compatible API surface
required for overlapping CIDRs and fixed-IP port creation is not yet wired.
The driver consumes these rows directly. This is a gate-only deviation and is
documented in the orchestrator script.

## WireGuard key provisioning

Per-host WireGuard keypairs are generated on the controller and the private key
is copied to each host's `O3K_NETWORK_P11_ROOT/wireguard-private.key`. Public
keys are used in `FabricHostIdentity`. This is acceptable as a test-lab
provisioning step because private keys never enter canonical O3K tenant state.

## Evidence outputs

- `/var/lib/o3k-fabric-lab/evidence/p11-gate-result.json`
- `/var/lib/o3k-fabric-lab/evidence/p11-evidence-network.json`
- `/var/lib/o3k-fabric-lab/evidence/p11-storage-evidence.json`
- `/var/lib/o3k-fabric-lab/evidence/p11-fake-hosts.json`
- `/var/lib/o3k-fabric-lab/evidence/p11-cleanup-inventory.json`

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
scripts/edge-fabric-gate.sh --dry-run
```

## Non-goals

- No changes to o3kd multi-host network dispatch.
- No OVN, OVS, eBPF, BGP, VXLAN, EVPN or custom datapath.
- No expansion of Neutron API compatibility.
- No new cloud features.
