# P12-IAM.3 — Scope Discovery and Authorized Rescoping

## Goal

Provide authoritative discovery of O3K scopes available to an authenticated federated principal and define safe project/domain/system rescoping behavior.

## Preconditions

- P12-IAM.2 merged.
- Start from latest protected `main`.

## Required behavior

Given a validated federated identity resolved to a canonical O3K principal, expose a native O3K contract that allows a confidential client to determine which scopes the principal may use.

The implementation must derive scopes from canonical O3K IAM assignments/policy inputs, not from client-supplied claims or Araf configuration.

Support only scope kinds actually accepted by the current O3K IAM architecture. Current kernel types may include project/domain/system; do not advertise a scope kind whose authorization semantics are not implemented and proven.

Each discoverable scope should provide the minimum stable client-facing metadata needed to select it, such as:

- scope ID;
- scope kind;
- safe display name if canonical/available;
- parent/domain metadata only when authoritative and safe;
- whether the current principal can request a native token for it.

The exact endpoint shape must follow the accepted P12-IAM specification and repository API conventions.

## Authorization rules

- discovery is principal-specific and authenticated;
- scopes without an effective assignment are omitted/denied;
- a client cannot gain access by supplying an arbitrary project ID;
- disabled scopes are unavailable;
- stale assignments fail closed;
- project and system authorization must remain distinct;
- no cross-tenant existence leakage through scope lookup errors.

## Rescoping

Define how a federated principal requests another authorized scope without browser reauthentication when the external credential remains valid. Rescoping must re-evaluate current O3K assignments and must not simply copy the previous AuthContext.

If native O3K token-to-token rescoping remains supported, specify its relationship to federated rescoping without changing existing semantics accidentally.

## Required tests

- Alice discovers Project A only;
- Bob discovers Project B only;
- Alice cannot discover/request Project B;
- arbitrary project ID is denied without existence leak;
- disabled project omitted/denied;
- removed assignment takes effect according to the accepted consistency model;
- duplicate display names do not affect selection by ID;
- system scope absent for normal tenant;
- system scope present only for explicitly authorized operator after P12-IAM.5;
- restart preserves scope truth;
- SQLite/PostgreSQL parity where persistence matters.

## Scale

Do not create an unbounded all-project enumeration path for large installations. If a principal may hold many assignments, use bounded server-side pagination consistent with the native API contract.

## Completion evidence

Publish the scope discovery/rescoping contract and negative isolation matrix.

Verdict:

`P12-IAM.3 scope discovery: PASS|BLOCKED`
