# ADR-0063 — Stop both services before reset cleanup

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The libvirt profile runs `o3kd` and `o3k-compute` against shared state. Reset
previously stopped only the control plane before deleting owned data and logs,
leaving the compute agent able to access removed state.

## Decision

When `systemctl` is available, reset stops `o3k-compute.service` and
`o3kd.service` before deleting owned contents. Stop failures remain best-effort,
matching the existing reset behavior, while the ordering prevents either
service from being intentionally left running by reset.

## Consequences

Fake-host packaging tests verify both stop requests. Reset remains safe for
non-system installations without `systemctl`, and credentials continue to be
preserved.
