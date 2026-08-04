# ADR-0014 — Deterministic Nova create retries

Status: Accepted for the durable fake/provider lifecycle foundation; provider-agent
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

dispatch and dependent-resource cleanup remain separate issue #47 work.

## Decision

The project ID and caller idempotency key deterministically derive the O3K
server and create-operation UUIDs. A retry with the same key is accepted only
when its complete create intent is byte-equivalent to the persisted intent; it
returns the existing server instead of creating a second resource. Reusing a
key with a different name, image, flavor, or network intent returns conflict.

The persisted SQLite resource remains the source of truth, so this behavior
survives a control-plane process restart without an in-memory idempotency map.

## Consequences

- Nova request retries cannot create duplicate server records or provider
  operations.
- The key is scoped by project and is never exposed as a host path or domain
  name; UUID derivation is one-way for practical operational purposes.
- Existing resources created before this behavior retain their IDs; callers
  should use a fresh request key when migrating an old in-flight request.
- Provider operation observation and full dependent-resource compensation still
  need the host-backed #47 workflow and release evidence.
