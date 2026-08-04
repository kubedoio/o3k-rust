# ADR-0113 — Require explicit benchmark release eligibility

Status: Accepted for the issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, governance

#91 release-evidence boundary.

## Context

The benchmark producer already records `release_eligible`, and fake or
control-plane-only runs set it to `false`. The release gate validated measured
libvirt-shaped fields but did not consume that declaration. A benchmark could
therefore be explicitly marked ineligible while still contributing to a ready
release report.

## Decision

The release gate requires `release_eligible: true` in both the benchmark summary
and raw artifact. It also requires the two values to match, in addition to the
existing raw digest and measured-field bindings.

## Consequences

An artifact cannot be promoted to release evidence by changing only its
summary status or measured fields. Fake and control-plane-only measurements
remain useful diagnostics and remain ineligible for the real-libvirt release
gate. This validates a producer declaration; it does not prove that a host,
guest, libvirt, or QEMU measurement actually occurred.

## Non-goals

This does not establish host identity, attest the producer, or claim that a
real-libvirt measurement exists. Those remain protected-workflow requirements.
