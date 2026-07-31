# ADR-0026 — TAP reuse ownership fencing

## Status

Accepted for the portable host-network safety slice.

## Context

The deterministic TAP name makes retries convenient, but a pre-existing
interface with that name may belong to another workload. Reapplying O3K's MAC
and bridge settings without checking ownership could mutate foreign host
network state.

## Decision

When a deterministic TAP already exists, `HostNetworkManager` reads its
read-only `ip` metadata and reuses it only when both the expected deterministic
MAC and requested O3K bridge membership are present. Any mismatch fails closed
with `ForeignInterface`. Newly created TAPs follow the existing setup path.

This is an ownership fence for reuse, not proof that the bridge itself is
O3K-owned or evidence from a privileged host. Bridge ownership, durable
interface manifests, DHCP/TAP orchestration, and libvirt XML attachment remain
separate integration work.

## Consequences

Retries no longer rewrite a same-named foreign or misattached interface. The
portable tests cover the metadata decision logic without requiring `ip` or
CAP_NET_ADMIN; privileged-host evidence remains explicitly pending.
