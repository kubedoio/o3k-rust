# ADR-0011: Selectable compute provider backends

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Decision

Nova compute uses a small `ProviderBackend` enum with fake and local-libvirt
implementations behind the existing provider contract. The libvirt backend
maps one O3K server to one stable `o3k-<hash>` domain, defines and starts the
domain, observes state through libvirt, and performs idempotent lifecycle
cleanup. The `provider` configuration accepts `fake`, `cellhv` (currently the
existing fake-compatible path), and `libvirt`.

## Rationale

Keeping the provider contract stable preserves reconciliation and unknown
outcome handling while allowing the control plane to select a real backend.
Libvirt errors are mapped to the provider's retryable, not-found, invalid, or
terminal categories at one boundary.

## Consequences

The current provider request carries image and compute resources but not full
network/config-drive data; those resources remain explicit follow-up wiring.
The libvirt feature still requires host libvirt development/runtime services.
Without them, the backend fails readiness/operations cleanly and does not
touch foreign domains.
