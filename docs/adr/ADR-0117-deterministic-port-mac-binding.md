# ADR-0117 — Persist one deterministic MAC binding per network port

Status: Accepted for the issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, placement, governance

#81 repository network-resource slice.

## Context

Port allocation already persisted a fixed IPv4 address, while host-network
helpers independently derived a MAC from a port reference. Without a persisted
API-visible binding, later TAP, libvirt, and DHCP orchestration could derive or
publish inconsistent identities.

## Decision

Derive a locally administered MAC (`02:` prefix) from the immutable port UUID,
persist it in `PortRecord`, migrate older records with an empty field on open,
and expose it as Neutron `mac_address`. The UUID remains the source of truth;
restarts return the same MAC and distinct ports receive distinct bindings.

## Evidence boundary and non-goals

This proves only deterministic control-plane identity and persistence. It does
not create a TAP, attach a libvirt NIC, configure dnsmasq, or prove guest IP
connectivity on a real host.

## Consequences

Future orchestration can consume one durable MAC binding across network, DHCP,
libvirt, and agent paths. Existing old metadata is upgraded without changing
port UUIDs or fixed addresses.
