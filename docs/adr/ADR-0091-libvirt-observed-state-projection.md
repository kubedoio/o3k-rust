# ADR-0091 — Fail closed on ambiguous libvirt lifecycle observations

## Status

Accepted for the portable issue #83 lifecycle-observation slice.

## Context

The libvirt adapter exposes both an `active` flag and libvirt's domain state.
Those values are not interchangeable. A paused, blocked, crashed, suspended,
or unknown domain can be active, while an inconsistent observation can report
an active flag that disagrees with the state value. Mapping only the active
flag makes those conditions appear as `Running` or `Stopped` and can cause the
control plane to make an unsafe lifecycle decision.

## Decision

Project only `(active=true, state=running)` to `Running`, and only an inactive
`shutdown` or `shutoff` observation to `Stopped`. All other known or unknown
states project to `Error` while retaining a redacted, human-readable provider
observation message. Reconciliation must observe and resolve that condition;
it must not infer a healthy state from an incomplete observation.

## Consequences

Portable tests now cover paused, crashed, blocked, suspended, unknown, and
inconsistent observations. This prevents a false healthy projection but does
not provide real libvirt, guest, Nova, or host acceptance evidence. Those
remain gated by the protected TestLab workflow.

## Public basis

- libvirt domain state values documented by the public `virDomainState`
  interface: <https://libvirt.org/html/libvirt-libvirt-domain.html#virDomainState>
- O3K [SPEC-0002](../specs/SPEC-0002-resource-state.md) requires observed
  state to be persisted and distinguishes it from public projections.
