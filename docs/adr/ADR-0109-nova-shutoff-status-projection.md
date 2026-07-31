# ADR-0109 — Project powered-off libvirt domains as Nova `SHUTOFF`

## Status

Accepted for the portable issue #83 observed-state slice.

## Context

The libvirt provider already projects an inactive `shutdown` or `shutoff`
observation to its internal `Stopped` state. The reconciler and compute API
serialized that internal name as `STOPPED`, which is not a Nova server status.
Clients therefore received a non-Nova status after a successful stop even
though the underlying observation was safe and correct.

## Decision

Keep `InstanceState::Stopped` as an internal provider state, but project it to
`SHUTOFF` at every Nova-facing boundary. Lifecycle validation accepts `SHUTOFF`
as the stopped state for start and reboot requests, and stop responses persist
`SHUTOFF`.

## Consequences

Portable reconciler and compute-service tests now prove the powered-off
projection. This change does not add agent dispatch, real guest execution,
restart recovery, or real-host acceptance evidence.

## Public basis

- OpenStack Nova server status documentation:
  <https://docs.openstack.org/nova/latest/reference/api-microversion-history.html>
- O3K [SPEC-0002](../specs/SPEC-0002-resource-state.md) distinguishes internal
  observed state from public projections.
