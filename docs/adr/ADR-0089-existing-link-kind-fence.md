# ADR-0089 — Existing bridge link-kind fence

## Status

Accepted for the portable issue #81 network safety slice.

## Context

The host network manager uses a configured interface name for the flat-network
bridge. Checking only whether a link with that name exists is insufficient: a
foreign physical, dummy, or other non-bridge link could occupy the name. The
previous path brought that link up before TAP attachment failed, which was an
unnecessary mutation of foreign host state.

## Decision

Before reusing an existing configured link, the manager performs a read-only
`ip -d link show` inspection and requires the output to contain the kernel
bridge kind marker. A missing command result or non-bridge link fails closed as
`ForeignInterface`, before any bridge state or uplink state is changed.

This validates link kind only. It does not claim bridge ownership, create TAPs,
attach libvirt interfaces, supervise DHCP, or provide privileged-host evidence.

## Consequences

An existing non-bridge link cannot be brought up or used as the O3K bridge.
The parser is tested with representative bridge and non-bridge output; actual
network mutation and guest connectivity remain host-gated issue #81 work.
