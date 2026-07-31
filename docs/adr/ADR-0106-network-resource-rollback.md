# ADR-0106 — Roll back O3K-created host-network resources

## Status

Accepted for the bounded portable issue #81 network-safety slice.

## Context

Host-network setup is a sequence of privileged `ip` commands. Before this
change, a bridge could remain after uplink attachment failed, and a TAP could
remain after address, bridge-master, or link-up setup failed. Retrying could
then encounter partial state, while blindly deleting by name could destroy a
pre-existing foreign interface.

## Decision

`HostNetworkManager` records whether the current operation created the bridge
and TAP. If a later command fails, it deletes only resources created by that
operation, in reverse dependency order: TAP first, then bridge. Existing
bridges and TAPs are never deletion targets. If cleanup itself fails, the
operation returns a distinct rollback error rather than claiming cleanup
succeeded.

The command boundary has a small injectable test implementation. Tests script
command results and assert the exact rollback sequence without requiring
privileged host networking.

## Consequences

Partial O3K-created network state is cleaned up deterministically, and foreign
interfaces remain protected by the existing bridge-kind and TAP-ownership
fences. A failed cleanup is observable and requires reconciliation or operator
action. This decision does not claim real-host bridge/TAP, guest connectivity,
DHCP, restart, or cleanup acceptance; those remain host-gated issue #81 work.

