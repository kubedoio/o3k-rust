# Contract Governance

O3K has two public contract families:

1. OpenStack-facing HTTP contracts under `contracts/openapi/`;
2. provider contracts under `proto/provider/`.

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
