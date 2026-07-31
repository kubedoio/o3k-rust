# Issue #81 — Network link-kind safety boundary

Issue #81 remains open and host-gated. Its full acceptance requires a real
CirrOS guest to obtain and use the assigned fixed IP through TAP, bridge,
libvirt, and managed DHCP lifecycle wiring, and depends on issue #78.

## Bounded repository slice

The portable network manager now rejects an existing configured link unless a
read-only `ip -d link show` inspection identifies it as a Linux bridge. This
prevents bringing up or otherwise reusing a foreign non-bridge link that merely
has the configured bridge name.

The manager now also tracks resources created by the current setup operation.
If a later bridge or TAP command fails, it rolls back the newly created TAP and
then bridge, while never deleting pre-existing foreign resources. A distinct
rollback error is returned if cleanup fails.

## Evidence

- `o3k-network` unit tests cover bridge-kind and non-bridge output parsing.
- deterministic command-injection tests cover uplink failure, TAP setup
  failure, exact reverse-order cleanup, and foreign TAP preservation;
  `ADR-0106` records the rollback decision.
- No privileged host networking, libvirt attachment, dnsmasq process, guest
  boot, or real-host acceptance is claimed.

## Remaining blockers

- agent-backed create path and issue #78 dependency;
- real TAP/bridge/libvirt NIC and DHCP orchestration;
- reboot/restart binding retention and cleanup evidence;
- trusted real-host artifact with `status: passed`.
