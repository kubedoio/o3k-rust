# ADR-0042 — Do not label libvirt preflight as lifecycle readiness

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, cli, governance

## Context

The real-libvirt script only checks host prerequisites. It did not create or
clean up a VM, yet its successful prerequisite branch wrote `status: ready`,
which could be mistaken for lifecycle evidence.

## Decision

All preflight-only outcomes, including a host with all prerequisites present,
are recorded as `status: skipped` until the lifecycle harness produces a
complete evidence artifact. The release-gate test asserts this distinction.

## Consequences

Evidence consumers cannot mistake preflight for a passed or ready lifecycle.
The script remains useful for host diagnostics, while real-libvirt acceptance
stays explicitly blocked until the harness exists.
