# SPEC-0011 — CellHV provider adapter

Status: Implemented adapter boundary; live environment pending

`o3k-cellhv` is an independently buildable tonic client for the versioned
`o3k.provider.v1` contract. It maps capabilities, create/get/delete, start,
stop, reboot, and operation polling into the Rust-native `ComputeProvider`
trait. HTTPS endpoints require CA, client certificate, and client key files;
the adapter does not log their contents or provider response payloads.

Capability version and required mutation capabilities are checked before a
mutation is accepted. Transport status codes map to the provider's typed
retryable, conflict, not-found, invalid, and terminal categories. Provider
operation and resource IDs remain opaque at the boundary.

The repository has no CellHV endpoint or credentials, so live VM creation,
timeout recovery against a real deployment, and Linux integration execution
remain environment-gated follow-ups. The adapter's TLS/configuration,
capability mismatch, redaction, wire mapping, and generated-contract tests run
without that external environment.
