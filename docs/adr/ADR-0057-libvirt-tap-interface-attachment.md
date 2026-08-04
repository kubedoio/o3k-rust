# ADR-0057 — Existing TAP attachment in libvirt domain XML

Status: Accepted for the libvirt XML boundary.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, placement, governance

## Context

The host network subsystem deterministically creates and owns TAP devices, but
the domain builder previously emitted no guest network interfaces. A successful
TAP allocation therefore could not reach a libvirt guest.

## Decision

`DomainSpec` accepts zero or more validated `DomainNetworkInterface` values.
Each value names an already-created O3K TAP and its deterministic MAC address.
The builder emits a libvirt `ethernet` interface with a virtio model and the
existing TAP as its target. TAP names must use the O3K prefix and interface
limits; MAC addresses must be six hexadecimal octets.

This boundary does not create or delete TAP devices, discover ownership, or
claim that a server create request currently carries the network attachments.
Those host execution, agent dispatch, and guest-connectivity steps remain
pending integration work.

## Consequences

Network XML cannot target an arbitrary host interface or inject malformed MAC
data. With a prepared owned TAP, libvirt receives a deterministic interface
definition without changing the no-network behavior of existing domains.
