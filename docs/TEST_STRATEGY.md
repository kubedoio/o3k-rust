# Test Strategy

## Goals

Tests provide evidence for compatibility, state safety, provider behavior, recovery, and user workflows. Test count and coverage percentage are secondary to meaningful behavioral evidence.

## Test layers

### 1. Domain tests

Fast tests for:

- resource ID and validation rules;
- lifecycle transition tables;
- quota and authorization decisions;
- idempotency and operation-state rules;
- serialization-independent invariants.

Prefer table-driven and property tests.

### 2. Protocol contract tests

For each supported OpenStack operation:

- request headers, query, path, and JSON validation;
- status code and response body;
- error envelope;
- token/project scope;
- advertised version and extensions;
- unsupported behavior.

These tests are black-box and should run against an in-process router and a real server process.

### 3. Store tests

The same behavioral suite runs against every store adapter. Required cases include transaction rollback, uniqueness, concurrent update, migration, restart, and corrupted/partial state handling.

### 4. Provider conformance tests

All compute/network/storage provider implementations must pass a common suite covering:

- capability reporting;
- create/read/delete idempotency;
- timeout with unknown outcome;
- duplicate command;
- partial completion;
- stale observation;
- provider unavailable;
- cleanup after failure.

### 5. Reconciliation tests

Use deterministic clocks and stateful fakes to test:

- restart after intent persistence;
- restart after external success but before persistence;
- bounded retry/backoff;
- terminal failure;
- orphan detection;
- no duplicate external resource creation.

### 6. End-to-end tests

The alpha gate runs a clean environment and executes:

```text
install -> token -> image -> network -> flavor -> server
-> restart -> show/list -> delete -> reset
```

Run the contract suite with the fake provider, then run the release gate with
libvirt/KVM through `o3k-compute` in a clean Linux integration environment.
CellHV coverage is optional follow-on provider conformance and does not replace
the libvirt release evidence.

### 7. Compatibility matrix

Each supported operation links to its executable tests and records known deviations. A route without evidence is unsupported regardless of implementation presence.

## Failure injection

Provider fakes must support scripted delay, timeout, unknown result, transient error, terminal error, stale state, and partial completion. Chaos is controlled and reproducible.

## Security testing

- authn/authz negative cases;
- tenant isolation;
- secret redaction;
- malformed and oversized input;
- path and object-store key safety;
- dependency and license checks;
- unsafe/native boundary tests when introduced.

## CI gates

Bootstrap CI requires formatting, clippy with warnings denied, the governed
nextest PR profile, doctests through cargo test, all workspace tests, and
documentation link checks when added. The pinned nextest profile uses zero
retries, fails flaky results, has deterministic slow/global timeouts, and emits
JUnit output. Later gates add cargo-deny, audit, OpenAPI validation, protobuf
compatibility, E2E, SBOM, and signed artifacts. The deeper profile is defined
but is not claimed as executed by the PR job.
