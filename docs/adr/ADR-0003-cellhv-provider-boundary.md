# ADR-0003 — CellHV through a provider contract

Status: Accepted

## Decision

O3K integrates with CellHV through a versioned protobuf provider contract. It does not import CellHV internal domain/store crates.

## Consequences

- independent repositories and release cycles;
- contract compatibility tests required;
- CellHV is preferred but not mandatory;
- shared utility crates require separate justification and versioning.
