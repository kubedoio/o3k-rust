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

The SDK exposes a generic `ServiceCompositionClient`. The database example
uses it to create deterministic child slots (`network-primary`, `volume-data`,
and `compute-primary`) using the parent resource and operation identity. Child
references use canonical typed resource references, and compensation traverses
known exclusive children in reverse order. The composition port is an
integration boundary; a production control-plane implementation must provide
authorization, delegation issuance, durable relationship persistence, and
transport. The example service does not access provider implementations or
private stores.

The current conformance fixture proves manifest loading, generic discovery,
typed child references, deterministic retry identities, and reverse
compensation ordering. It does not yet constitute the full process-level
P12.6 acceptance proof: durable relationship persistence, a live composition
adapter, and the authenticated end-to-end failure matrix remain required.
A production DBaaS claim, PostgreSQL lifecycle, and P12.7 security/evidence
convergence remain explicitly out of scope.
