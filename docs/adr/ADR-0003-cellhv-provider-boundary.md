# ADR-0003 — CellHV through a provider contract

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, governance

## Decision

O3K integrates with CellHV through a versioned protobuf provider contract. It does not import CellHV internal domain/store crates.

## Consequences

- independent repositories and release cycles;
- contract compatibility tests required;
- CellHV is optional and independently releasable; libvirt/KVM is the primary
  real backend for `v0.2.0-alpha.1` (see ADR-0005);
- shared utility crates require separate justification and versioning.
