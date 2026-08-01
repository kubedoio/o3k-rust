# SPEC-0003 — Provider Boundary

Status: Implemented compute subset

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
- a partial create may return `RUNNING` with a provider resource ID while the
  instance is still `CREATING`; the resource must be observed before the
  operation is reported as successful;
- timeout returns unknown outcome when acceptance cannot be disproved;
- provider errors map to typed categories;
- provider-specific debug details are retained internally and redacted publicly;
- capability discovery is explicit and versioned.

## Rust port and fake

`o3k-provider` defines the Rust-native `ComputeProvider` trait independently
from protobuf transport. Its initial subset covers capabilities, instance
create/get/delete, and operation lookup. `FakeComputeProvider` is stateful and
supports idempotency-key replay, idempotent deletion of absent instances, and
failure injection for transient, terminal, timeout/unknown-outcome, stale
state, and partial-completion scenarios. `run_conformance` is reusable by
future provider adapters.

Successful and unknown operations retain an optional provider resource ID so a
reconciler can observe the resource after a lost response without issuing a
duplicate create.

Partial completion is recoverable: repeated delivery with the same idempotency
key returns the original operation and resource, and a later observation may
converge the instance to `RUNNING`. A provider must not require a second create
to complete that transition.

Provider errors intentionally expose categories and operation identity only;
provider-specific payloads and diagnostics are not part of the public error.

## CellHV

CellHV is an optional provider behind this contract. Libvirt/KVM is the primary
real backend for `v0.2.0-alpha.1`; O3K depends on public provider contracts, not
CellHV internal crates or database schemas.
