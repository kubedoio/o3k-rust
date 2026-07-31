# Issue #84 — Console observation ownership fence

Issue #84 asks for actual bounded libvirt serial output through Nova's
console-log operation. The full issue remains host-gated: this repository has
no evidence of CirrOS boot output or a trusted real-host artifact.

## Bounded repository implementation

The libvirt compute agent now inspects the derived domain and verifies its O3K
metadata and server ID before opening a serial console stream. Missing,
malformed, or cross-server metadata is rejected, matching the ownership fence
used by lifecycle actions. A regression test covers matching, mismatched, and
missing metadata.

## Decision for this bounded slice

After the authenticated API has completed `ComputeService::delete_server`, it
asks the configured `ConsoleService` to remove the console artifact for that
same project-owned server UUID. Cleanup is idempotent. A failed compute delete
returns its existing conflict response and does not invoke cleanup, preserving
the artifact for retry or diagnosis. Cleanup is limited to the UUID-derived
O3K console path; it makes no host or libvirt ownership claim.

Regression coverage exercises failed deletion preservation, successful
deletion cleanup, and repeated deletion/cleanup.

## Explicit non-goals

- no claim of real guest console output;
- no change to serial-device XML or stream transport;
- no change to Nova API bounds or persisted console retention;
- no substitute for protected real-host and OpenStack CLI evidence.

Decision: ADR-0092 for the provider console ownership fence; ADR-0110 for
successful Nova deletion cleanup.
