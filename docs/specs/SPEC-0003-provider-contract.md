# SPEC-0003 — Provider Boundary

Status: Draft

## Purpose

Providers execute infrastructure actions while O3K retains OpenStack semantics, desired state, policy, and reconciliation.

## Initial provider capabilities

- compute instance create/get/list-actions/start/stop/reboot/delete;
- capability and capacity reporting;
- operation lookup;
- request correlation and idempotency identity.

Network and storage contracts will be added only with their vertical slices.

## Rules

- provider IDs are opaque;
- commands include an O3K operation ID;
- provider reports accepted operation identity;
- synchronous success does not replace later observation;
- timeout returns unknown outcome when acceptance cannot be disproved;
- provider errors map to typed categories;
- provider-specific debug details are retained internally and redacted publicly;
- capability discovery is explicit and versioned.

## CellHV

CellHV is the preferred first real provider. O3K depends on the public protobuf contract, not CellHV internal crates or database schemas.
