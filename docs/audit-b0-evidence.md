# Durable Audit B0 executable evidence

Issue #902 / PR #926 (`integration/durable-audit`). The B0 verifiers execute
the following production-boundary tests; they fail closed on assertion failure.

| Evidence | Executable proof |
|---|---|
| B0-S08..S19, S22..S25 | `durable_audit_b0_runtime_matrix_covers_mandatory_paths_and_recovery` exercises canonical mandatory outcomes, denial/failure/unknown-outcome recording, replay idempotency, restart reconstruction, concurrent writers, unavailable-repository failure, and secret-safe serialization through `DurableAuditSink` and the durable store. |
| B0-S21 | PostgreSQL repository/conformance and migration suites in `crates/o3k-store/tests/postgres_audit_repository.rs`, run by the repository gate, cover the equivalent production adapter. |
| B0-A03..A05, A09..A12, A15..A18 | `audit_api_b0_contract_matrix` drives the mounted native router with bearer authentication and a durable-reader-shaped adapter, proving filtering, isolation, pagination/order, cursor/filter binding, malformed input, missing resources, and Problem Details failures. |
| B0-A01, A02, A06..A08, A13, A14 | Native API suite plus the same matrix prove route mounting, bounded pages, opaque authenticated cursors, operation/correlation representation, and secret-safe DTOs. |

The source-level sink and API verifiers invoke these tests directly; no B0
requirement is represented by a `NOT PROVEN` placeholder.
