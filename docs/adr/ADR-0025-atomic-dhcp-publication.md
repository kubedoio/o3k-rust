# ADR-0025 — Atomic DHCP state publication

## Status

Accepted for the portable DHCP configuration slice.

## Context

The DHCP service already used rename-based publication, but every write used
the same `.tmp` pathname. Concurrent state/config writers could overwrite one
another's temporary bytes, and a failed write could leave stale temporary
output for a later process to mistake for current state.

## Decision

Each atomic write uses a unique managed temporary filename, removes it on both
write and rename failure, and publishes only through a successful rename. The
durable state and rendered dnsmasq configuration retain their existing
validation and ownership root.

This slice does not add dnsmasq process supervision, restart/reload handling,
TAP/libvirt integration, or privileged-host DHCP evidence.

## Consequences

Concurrent writers no longer share a temporary pathname, and failed writes do
not leave stale temporary files behind. Process lifecycle and host integration
remain explicit follow-up work.
