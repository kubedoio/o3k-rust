# ADR-0002 — TestLab first

Status: Accepted

## Decision

The first product is a single-node TestLab, not a production OpenStack replacement.

## Rationale

A complete small workflow provides earlier user value, faster compatibility feedback, and a measurable base for edge and SMB profiles.

## Consequences

- SQLite, local image storage, flat networking, and fake/libvirt providers
  first;
- libvirt/KVM is the primary real backend for `v0.2.0-alpha.1`; CellHV is an
  optional later provider behind the same boundary;
- endpoint breadth is deferred;
- production claims require separate evidence and ADRs.
