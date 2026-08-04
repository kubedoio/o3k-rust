# ADR-0019 — Canonical create-command construction

Status: Accepted for the alpha command-routing slice.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: identity, governance

## Context

The command router could deliver arbitrary protocol commands, but there was no
single control-plane construction path guaranteeing stable identity and a
canonical payload fingerprint. Retries could otherwise produce different
command IDs or silently change the semantic payload.

## Decision

`build_create_command` accepts a typed `CreateCommandSpec`, validates all
identities, references, and deadline, derives a deterministic UUID-v5 command
ID from agent and operation identity, and fingerprints the encoded
`CanonicalCommandPayload` with SHA-256. The same spec produces byte-equivalent
command metadata; changing image, flavor, or network references changes the
fingerprint while preserving the operation-scoped command identity.

## Consequences

Create commands are ready to be dispatched through the authenticated router
without embedding secrets, XML, shell text, or arbitrary host paths. This is a
construction boundary only: connecting it to the durable ComputeService
operation journal and mapping agent events back to operations remains the next
integration step.
