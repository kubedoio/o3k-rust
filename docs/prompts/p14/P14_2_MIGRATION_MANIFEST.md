# P14.2 — Versioned Migration Manifest

Implement only SPEC-0037 manifest v1 and durable source-to-destination mapping.
Validate schema, digest, ownership scope, dependency graph, stable ordering,
unsupported-field decisions, and deterministic operation identity. Reject
secrets and unknown authority fields. Test replay, tampering, duplicate source
keys, changed snapshots, and restart reconstruction.
