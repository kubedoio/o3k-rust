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
| Volume canonical owner/read path | `o3kd::native_adapters::volume_reader_tests`; `o3k-store` P12.4 canonical-resource tests |
| Duplicate authority | Executable convergence tests prove shared canonical authority for the exercised Compute/Network paths; architecture inspection confirms no second native/OpenStack synchronization store exists |
| Restart relationship | `p12_6_reconstructs_two_independent_control_plane_runtimes`; `p12_6_relationship_recovery_reopens_store_and_serializes_process_race`; post-restart HTTP convergence remains NOT PROVEN |

The selected OpenStack compatibility tests exercise the same application
services used by the native adapters. The process-level convergence test
exercises both HTTP directions for Compute and Network, including deletion.
Native Volume has no supported OpenStack-compatible endpoint in the advertised
profile, so a Volume native/OpenStack pair is NOT APPLICABLE; external Cinder
remains an explicitly separate authority under the accepted architecture.

## Native API security (SPEC-0030 §17)

| Property | Executable evidence |
| --- | --- |
| Missing/malformed/invalid bearer and no mutation | `o3kd::native_adapters::native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation` |
| Cross-project isolation / IDOR / operation isolation | same test; `operation_visibility_tests::operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields` |
| Unknown namespace/resource fails closed | `native_security_rejects_auth_namespace_and_cross_scope_access_before_mutation`; `resource::tests::different_resource_types_share_one_registry_resolution_path` |
| Idempotency replay, conflict, cross-scope isolation, and provider-side-effect bound | `native_compute_create_replay_equivalent`; `native_compute_create_changed_body_conflict`; `native_compute_idempotency_isolated_between_owner_scopes` |
| Cursor tampering, wrong key, malformed cursor, page-size bounds | `o3k_native_api::pagination::tests` |
| Owner scope is derived from authenticated context | `o3k_api::two_tenant_isolation::two_tenant_path_and_resource_isolation`; native security test |
| Secret-safe public operation errors | `operation_visibility_tests::operation_route_is_store_backed_owner_scoped_and_redacts_provider_fields`; `o3k-store::postgres_ops::test_postgres_error_mapping_and_no_leakage` |
| Generation/CAS and authorization-before-side-effect | `o3k-store` SQLite/PostgreSQL P12.4 lifecycle and race tests; native rejected-request provider count assertion |
| Update semantics | NOT APPLICABLE: no native generic update route is advertised in SPEC-0030 v1 |

## Controller and extension security (SPEC-0031 §22)

| Property | Executable evidence |
| --- | --- |
| Manifest validation, namespace/action conflicts, generic discovery | `p12_6_process::p12_6_reconstructs_two_independent_control_plane_runtimes`; manifest registry/unit validation tests |
| Authenticated controller identity and mTLS boundary | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries` |
| Stale-session fencing / reconnect | `p12_6_process::p12_6_reconstructs_two_independent_control_plane_runtimes`; `unavailable_external_controller_and_composition_endpoints_fail_closed` |
| Delegated actor, owner scope, action, target, and parent operation | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries` |
| Unknown outcome, replay-safe reconciliation, compensation, cleanup | `p12_6_process::database_controller_and_composition_cross_real_mtls_boundaries`; `p12_6_independent_application_instances_converge_durable_slots` |
| Unsafe service removal | NOT PROVEN: `ManifestRegistry::remove` exists, but P12.7 has not yet exercised removal with owned resources, in-flight Operations, or dependencies |
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

## SPEC-0030 §20 final gates

1. namespaced discovery — PASS (`native_api` discovery/resource tests)
2. canonical IAM/authorization/ownership — PASS (native security and two-tenant tests)
3. Compute/Network/Volume reads — PASS (native adapter and API lifecycle tests)
4. correct mutation status codes — PASS (native mutation tests and P12.6 process test)
5. service-neutral Operation — PASS (operation visibility and process tests)
6. idempotency/generation safety — NOT PROVEN (native cross-scope idempotency is covered; native generation preconditions are not advertised, and store-only generation tests do not close the API gate)
7. opaque pagination scope safety — NOT PROVEN (HTTP native traversal and cross-owner cursor tests remain required)
8. SQLite/PostgreSQL conformance — NOT PROVEN (store-level PostgreSQL tests executed; native API conformance against PostgreSQL remains required)
9. selected OpenStack regression — PASS (`o3k-api` isolation/lifecycle suites)
10. one canonical native/OpenStack authority — PASS for exercised Compute/Network paths (shared application/store wiring and lifecycle evidence)

## SPEC-0031 §24 final gates

1. validated ServiceManifest — PASS
2. namespace/resource/action conflict rejection — PASS
3. separate compatibility projection — PASS
4. authenticated/versioned external controller — PASS
5. bounded actor/scope-preserving delegation — NOT PROVEN (the required independent negative delegation cases are not all cited)
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
`o3k-api` isolation/lifecycle tests, `p12_6_process`, and SQLite/PostgreSQL
store tests. Protected-host or real-provider evidence is not claimed here.
