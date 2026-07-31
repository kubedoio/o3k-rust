# Contract Governance

O3K has three public contract families:

1. OpenStack-facing HTTP contracts under `contracts/openapi/`;
2. provider contracts under `proto/provider/`;
3. the compute-host boundary draft under `proto/compute/`.

## HTTP contracts

OpenAPI files document only the subset O3K intentionally supports. They are not generated claims of full OpenStack compatibility. Executable tests remain the acceptance authority.

A contract change must state whether it is:

- additive and backward compatible;
- behavior clarification;
- intentional incompatibility requiring versioning and release notes.

## Protobuf contracts

- never reuse a field number;
- reserve removed field names and numbers;
- use explicit capability discovery;
- represent long-running mutations as operations;
- separate requested, accepted, observed, and terminal states;
- avoid provider-specific values in the common contract unless placed in a typed extension;
- run protobuf breaking-change checks before merge.

The `o3k-provider-contract` crate generates Rust clients and servers from the
committed files under `proto/provider/` during every clean build. Generated
code is build output and is not hand-edited or committed. The Rust-native
`o3k-provider` types remain independent; `mapping` is the explicit transport
boundary. CI must run `cargo fmt --all -- --check`, protobuf generation through
`cargo test --workspace`, descriptor validation, and `buf breaking` against the
default branch. The initial compute-agent package is additive relative to the
existing provider package, so the first comparison establishes the baseline;
future incompatible changes require a new package version.

Contract versioning is package-based (`o3k.provider.v1` and
`o3k.compute.v1`). Additive fields are compatible; removed fields must be
reserved and field numbers are never reused. A behavior or wire incompatibility
requires a new package version. The compute-agent draft is specified in
[`SPEC-0015`](../specs/SPEC-0015-compute-agent.md) and its boundary decision in
[`ADR-0006`](../adr/ADR-0006-compute-agent-boundary.md); it is not generated
into the runtime workspace until the agent implementation issue is approved.

## Error model

Errors must distinguish:

- invalid request;
- unauthenticated/unauthorized;
- conflict/invalid state;
- quota/capacity;
- not found;
- retryable provider error;
- unknown provider outcome;
- terminal provider error;
- internal invariant violation.

Do not leak provider secrets or raw internal errors to public clients.
