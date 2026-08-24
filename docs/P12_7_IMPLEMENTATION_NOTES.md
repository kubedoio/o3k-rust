# P12.7 / #735 implementation evidence

This note records executable evidence for the final P12 convergence stage. A
line is marked PASS only when the named test executes the behavior. The real
PostgreSQL P12.3/P12.4 suite was executed against the dedicated `o3k_p12_7`
database with `O3K_DATABASE_URL`.

## Native/OpenStack authority convergence

| Property | Executable evidence |
| --- | --- |
| Compute canonical identity and owner | `p12_7_convergence::native_and_openstack_http_surfaces_share_compute_and_network_authority`; `o3kd::native_adapters::native_compute_create_and_read_operation` |
| Native compute idempotent canonical mutation | `o3kd::native_adapters::native_compute_create_replay_equivalent`; `native_compute_create_changed_body_conflict` |
| Network canonical owner/read path | `p12_7_convergence::native_and_openstack_http_surfaces_share_compute_and_network_authority`; `o3k_api::health::neutron_network_subnet_port_lifecycle_is_deterministic` |
| OpenStack foreign Network dependency authorization | `p12_7_convergence::native_and_openstack_http_surfaces_share_compute_and_network_authority` creates a real project-b Network/Port, attempts Nova create from project-a, and proves a concealed 404 with zero provider mutation |
| Volume canonical owner/read path | `o3kd::native_adapters::volume_reader_tests`; `o3k-store` P12.4 canonical-resource tests |
| Duplicate authority | Executable convergence tests prove shared canonical authority for the exercised Compute/Network paths; architecture inspection confirms no second native/OpenStack synchronization store exists |
| Restart relationship | `p12_6_reconstructs_two_independent_control_plane_runtimes`; `p12_6_relationship_recovery_reopens_store_and_serializes_process_race`; the shared `run_http_restart_conformance` body proves fresh HTTP runtime reconstruction over durable SQLite and real PostgreSQL |

The selected OpenStack compatibility tests exercise the same application
services used by the native adapters. The process-level convergence test
exercises both HTTP directions for Compute and Network, including deletion.
Native Volume has no supported OpenStack-compatible endpoint in the advertised
profile, so a Volume native/OpenStack pair is NOT APPLICABLE; external Cinder
remains an explicitly separate authority under the accepted architecture.

Generation applicability is structurally exercised by
`native_adapters::native_compute_manifest_exposes_no_generation_precondition_mutation`.
Native Compute's advertised manifest operations are
`list`, `show`, `create`, and `delete`; no native update or compare-and-set
route/header/request field is advertised in SPEC-0030 v1. Resources expose
generation metadata and the store has CAS semantics, but API-level stale
generation testing is not applicable until a mutation precondition is
advertised.

## Native API security (SPEC-0030 §17)

| Property | Executable evidence |
| --- | --- |
| Missing/malformed/invalid bearer and no mutation | `o3kd::native_adapters::native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation` |
| Cross-project isolation / IDOR / operation isolation | same test; `operation_visibility_tests::operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields`; `native_compute_rejects_foreign_network_before_provider_mutation` |
| Unknown namespace/resource fails closed | `native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation`; `resource::tests::different_resource_types_share_one_registry_resolution_path` |
| Idempotency replay, conflict, cross-scope isolation, and provider-side-effect bound | `native_compute_create_replay_equivalent`; `native_compute_create_changed_body_conflict`; `native_compute_idempotency_isolated_between_owner_scopes` |
| Cursor traversal, tampering, owner binding, and stale-anchor handling | `p12_7_convergence::native_and_openstack_http_surfaces_share_compute_and_network_authority`; `native_adapters::native_http_cursor_is_bound_to_owner_and_rejects_tampering`; `native_adapters::native_http_cursor_continues_deterministically_after_anchor_deletion`; codec boundary tests in `o3k_native_api::pagination::tests` |
| Owner scope is derived from authenticated context | `o3k_api::two_tenant_isolation::two_tenant_path_and_resource_isolation`; `p12_7_convergence::native_http_scope_like_request_fields_cannot_select_foreign_owner` rejects top-level and typed scope injection and verifies the resulting resource remains owned by the authenticated project |
| Secret-safe public operation errors | `operation_visibility_tests::operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields`; `o3k-store::postgres_ops::test_postgres_error_mapping_and_no_leakage` |
| Generation/CAS and authorization-before-side-effect | `native_compute_manifest_exposes_no_generation_precondition_mutation` proves no advertised native mutation accepts CAS; `o3k-store` SQLite/PostgreSQL P12.4 lifecycle and race tests prove generation safety at the store layer; `native_compute_rejects_foreign_network_before_provider_mutation` proves authorization precedes provider mutation |
| Malformed JSON | `native_adapters::native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation` |
| Oversized JSON | `native_adapters::native_http_oversized_body_is_rejected_before_provider_mutation` exercises the advertised 1 MiB Axum body limit and proves HTTP 413 with zero provider mutation |
| Concrete route fail-closed behavior | `native_adapters::native_http_route_shapes_fail_closed_without_descriptor_dispatch` exercises future, extra-segment, encoded, and wrong-method routes with no provider mutation |
| Update semantics | NOT APPLICABLE: no native generic update route is advertised in SPEC-0030 v1 |

## Controller and extension security (SPEC-0031 §22)

| Property | Executable evidence |
| --- | --- |
| Manifest validation, namespace/action conflicts, generic discovery | `p12_6_process::p12_6_reconstructs_two_independent_control_plane_runtimes`; manifest registry/unit validation tests |
| Authenticated controller identity and mTLS boundary | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries` |
| Stale-session fencing / reconnect | `p12_6_process::p12_6_reconstructs_two_independent_control_plane_runtimes`; `unavailable_external_controller_and_composition_endpoints_fail_closed` |
| Delegated actor, owner scope, action, target, and parent operation | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries` exercises valid mTLS composition plus wrong action, owner scope, parent operation, service principal, expired delegation, stale session, and a real tenant-B child observation attempt through the composition boundary; each rejected case leaves no relationship |
| Unknown outcome, replay-safe reconciliation, compensation, cleanup | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries`; `p12_6_independent_application_instances_converge_durable_slots` |
| Unsafe service removal | `manifest::tests::service_authority_cannot_be_forgotten_by_registry_removal`; registry removal fails closed and controller removal retains the manifest |
| Separate compatibility projection/evidence gating | compatibility inventory/target validation tests and `docs/compatibility/` manifests; no metadata-only capability is advertised |

## Database conformance service

`database_controller_and_composition_cross_real_mtls_boundaries` proves
manifest registration, generic discovery/mutation, authenticated controller
identity, delegated Compute/Network/Volume composition, durable relationships,
operation correlation, restart/reconnect fencing, compensation, and cleanup.
`o3k-kernel` contains no Database-specific business logic; the behavior is in
`crates/o3k-database-example` and the generic composition/application ports.

## Persistence

SQLite evidence is provided by the P12.4 lifecycle tests and P12.6 process
tests. PostgreSQL evidence is provided by
`crates/o3k-store/tests/postgres_p12_4.rs` and `postgres_ops.rs` against the
real PostgreSQL harness. The PostgreSQL tests are never replaced by a mock.
The same HTTP/application conformance body is also exposed through
`p12_7_convergence::native_and_openstack_http_surfaces_share_compute_and_network_authority`
for SQLite and
`native_and_openstack_http_surfaces_share_compute_and_network_authority_postgres`
for PostgreSQL; the latter was executed with
`O3K_DATABASE_URL=postgres://o3k:password@127.0.0.1/o3k_p12_7 cargo test -p o3kd --test p12_7_convergence --all-features -- --ignored`.
The same backend-parameterized restart body is executed by
`native_and_openstack_http_surfaces_reconstruct_over_durable_postgres`.

## SPEC-0030 §20 final gates

1. namespaced discovery — PASS (`native_api` discovery/resource tests)
2. canonical IAM/authorization/ownership — PASS (native security and two-tenant tests)
3. Compute/Network/Volume reads — PASS (native adapter and API lifecycle tests)
4. correct mutation status codes — PASS (native mutation tests and P12.6 process test)
5. service-neutral Operation — PASS (operation visibility and process tests)
6. idempotency/generation safety — PASS (`native_compute_manifest_exposes_no_generation_precondition_mutation` proves the advertised v1 mutation surface has no compare-and-set endpoint; SPEC-0030 §10 makes preconditions conditional on such an endpoint; native idempotency replay/conflict/isolation tests and SQLite/PostgreSQL store generation/CAS tests prove the applicable safety layers)
7. opaque pagination scope safety — PASS (`p12_7_convergence` HTTP traversal/tampering, `native_http_cursor_is_bound_to_owner_and_rejects_tampering` cross-owner binding, and `native_http_cursor_continues_deterministically_after_anchor_deletion` stale-anchor rejection)
8. SQLite/PostgreSQL conformance — PASS (the same native HTTP/application semantic body executes against SQLite and real PostgreSQL, including `native_and_openstack_http_surfaces_reconstruct_over_durable_sqlite` and `native_and_openstack_http_surfaces_reconstruct_over_durable_postgres`; the PostgreSQL run used the real `O3K_DATABASE_URL` harness)
9. selected OpenStack regression — PASS (`o3k-api` isolation/lifecycle suites)
10. one canonical native/OpenStack authority — PASS for exercised Compute/Network paths (fully wired HTTP convergence, deletion, shared application/store wiring, and lifecycle evidence)

## SPEC-0031 §24 final gates

1. validated ServiceManifest — PASS
2. namespace/resource/action conflict rejection — PASS
3. separate compatibility projection — PASS
4. authenticated/versioned external controller — PASS
5. bounded actor/scope-preserving delegation — PASS (`p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries` independently rejects wrong action, wrong owner scope, wrong parent operation, wrong service principal, expired delegation, stale session, and observation of a real tenant-B child through the real mTLS composition boundary, with no durable relationship created)
6. generic discovery and CLI — PASS (P12.6 process/CLI coverage)
7. canonical cross-service composition — PASS
8. durable operation/audit correlation and unknown outcome — PASS
9. restart/reconnect stale-session fencing — PASS
10. no Database-specific Cloud Kernel logic — PASS (architecture boundary plus process evidence)

## Validation commands

The required completion gate is:

```text
python3 scripts/check-architecture-boundaries.py
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

P12.7-specific portable suites include the native adapter tests, the
`p12_7_convergence` HTTP suite, the `o3k-api` isolation/lifecycle tests,
`p12_6_process`, and SQLite/PostgreSQL store tests. The convergence suite now
uses the fully wired Compute/Network application and exercises native HTTP
pagination traversal and cursor tampering. Protected-host or real-provider
evidence is not claimed here.
