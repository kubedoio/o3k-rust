# SPEC-0017 — OpenAPI contract governance

Status: Proposed. This specification becomes normative only after human
acceptance through the ADR lifecycle in
[ADR-0154](../adr/ADR-0154-engineering-governance-lifecycle.md).

## Purpose and scope

This specification governs the repository's public HTTP contract descriptions.
It applies to `contracts/openapi/` and to any referenced schema, example, or
generated documentation. It does not claim that the current bootstrap
description satisfies these requirements, and it does not change the static
OpenStack compatibility manifest or API baseline.

## Contract policy

- New or materially changed public HTTP behavior starts with a reviewed
  contract change. Handler implementation is not the source of truth for a
  missing operation.
- The target document version is OpenAPI `3.1.2`, with JSON Schema 2020-12
  semantics as defined by that OpenAPI version. Existing documents remain at
  their committed version until a migration PR passes the checks below; this
  proposal must not be read as evidence that migration has happened.
- Every operation has a stable, repository-unique `operationId`. Renaming an
  operation ID is a compatibility change and requires an explicit decision.
- Parameters, request bodies, response bodies, status codes, security
  requirements, pagination, and relevant request/response headers are explicit
  rather than implied by an implementation.
- OpenStack version discovery, service-qualified version headers, supported
  microversion windows, and unsupported operations are represented in the
  contract or in a linked normative specification. A codename alone is not a
  wire contract.
- Reusable schemas and examples are preferred. Examples are test inputs, not
  prose claims: each example must validate against its referenced schema and
  use redacted, deterministic data.
- Error responses use the service's documented OpenStack envelope and include
  the status and authentication/scope behavior required by the applicable
  baseline.
- Body-size, string-length, array-size, pagination, and filter limits are
  explicit wherever untrusted input crosses the public boundary.
- `x-o3k-*` extensions may describe compatibility metadata, but extensions
  must not weaken the normative OpenAPI operation, schema, or security rules.

## Change and compatibility rules

Every contract PR records:

1. the affected operation IDs and paths;
2. the linked requirement, specification, or issue;
3. whether the change is additive, backward-compatible, or breaking;
4. the request/response examples and negative cases changed;
5. the exact validation commands and their results;
6. the evidence state of the behavior. A contract edit alone is not runtime
   or real-host evidence.

A breaking change requires a new API/microversion decision or an accepted ADR
before merge. Removing an operation, weakening validation, changing an error
status/envelope, changing scope rules, or changing version negotiation is
breaking even when the path remains present.

## Required acceptance checks

The eventual OpenAPI gate must fail closed when any of these conditions holds:

- a document is syntactically invalid, has unresolved references, or declares
  an unsupported OpenAPI version;
- a path/method or `operationId` is duplicated;
- a path template has no matching required parameter;
- an example fails its schema, contains a secret, or depends on nondeterministic
  identifiers without a normalization rule;
- a supported operation lacks an explicit success/error response or required
  security/header declaration;
- the contract exposes a route not present in the approved TestLab surface, or
  omits a baseline-required operation, without a reviewed specification change;
- a compatibility diff is breaking without the required ADR or microversion
  decision;
- generated documentation or fixtures are stale relative to the source
  contract.

The implementation may use different lint and validation tools, but the gate
must report the document paths, operation IDs, and failing rule. A green
syntax check alone is insufficient evidence for semantic compatibility.

## Migration acceptance record

An OpenAPI migration PR must attach a machine-readable result containing the
source commit, tool versions, document digests, validation command, and
pass/fail result. Until such an artifact exists, the migration status is
`missing`, not `verified`.

## Public source

- [OpenAPI Specification 3.1.2](https://spec.openapis.org/oas/v3.1.2.html),
  accessed 2026-08-02. The specification defines the OAS document version and
  its 3.1 schema dialect; it is the normative source for this policy's OpenAPI
  format choice.
- [O3K TestLab API baseline](TESTLAB_API_BASELINE.md)
- [Repository clean implementation policy](../CLEAN_IMPLEMENTATION.md)
