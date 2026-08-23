# P12.6 implementation notes

P12.6 uses the generic service-extension primitives established by P12.5.
The database conformance service is described by the external
`crates/o3k-database-example/service-manifest.json` artifact and declares an
external gRPC controller plus bounded compute, network, and volume dependency
actions. It is not production DBaaS and does not install or manage PostgreSQL.

`ManifestRegistry::register_json_file` and
`ManifestRegistry::register_json_directory` are service-neutral runtime
loading paths. They parse the accepted ServiceManifest v1 wire shape and
perform normal registry validation; no service namespace is special-cased.

The SDK exposes a generic `ServiceCompositionClient` and the versioned
`o3k.composition.v1` gRPC service. O3K's composition adapter authenticates the
configured service principal, verifies the O3K-issued parent delegation, checks
the manifest dependency surface, resolves lifecycle actions from registered
resource descriptors, reserves generic relationship slots before child side
effects, and binds canonical child operation/resource receipts. Relationship
records are generic and have reserved, bound, deleting, deleted, and unknown
states in SQLite and PostgreSQL store implementations. The Database example
does not access provider implementations or private stores.

The example also contains a standalone `database-controller` process entry
point. It uses the P12.5 controller server and a real gRPC/mTLS composition
client, reconstructs child state from the relationship ledger after process
loss, derives status by observing children, and compensates exclusive children
in reverse order. Runtime endpoint, certificate, key, digest, and delegation
verification configuration remain deployment settings, separate from the
manifest.

The current automated evidence proves manifest loading, generic discovery,
typed child references, deterministic retry identities, descriptor-authoritative
child lifecycle resolution, generic relationship persistence primitives, and
the standalone controller/client build. The full process-level P12.6
acceptance proof is still pending: a harness must run O3K and the controller
against real local mTLS listeners, exercise authenticated generic parent
create/show/delete, and cover restart, unknown-outcome, quota, compensation,
and concurrent-reconcile evidence across the complete path. This note must not
be read as a production support claim.
A production DBaaS claim, PostgreSQL lifecycle, and P12.7 security/evidence
convergence remain explicitly out of scope.
