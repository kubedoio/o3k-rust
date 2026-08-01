# ADR-0121 — Recompute benchmark target results at the release gate

## Status

Accepted for the repository-side preparation of issue #91. This does not claim
that real-host measurements exist or pass.

## Context

The benchmark summary carried a `targets_evaluated` map, but the release gate
only checked that its values were truthy. A summary could therefore claim
passing targets while its digest-bound raw measurements exceeded the declared
thresholds.

## Decision

The release gate recomputes the three target results from the raw control-plane
measurements and thresholds: startup readiness in milliseconds, idle RSS in
KiB versus its MiB limit, and token p95 in milliseconds versus its seconds
measurement. It rejects non-numeric measurements and any summary map that does
not exactly match the recomputed result.

## Consequences

The summary remains cryptographically bound to the raw document and its target
claims are independently checked. Real guest/libvirt measurements and host
metadata remain required before release evidence can pass.
