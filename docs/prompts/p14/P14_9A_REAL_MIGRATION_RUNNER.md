# P14.9A — Real migration runner prerequisite

P14.9A wires the accepted P14.1–P14.8 contracts into a process-level
workflow. It is a prerequisite for, and does not close, P14.9 real two-cloud
acceptance.

## Boundaries

- `o3k-migration::runner::MigrationRunner` owns dependency ordering, durable
  manifest projections, observation-before-replay, restart/resume, owned-only
  rollback, and one-way cutover fencing.
- `CanonicalDestination` is the application/API boundary. The provided
  `HttpNativeDestination` calls the O3K native API and never writes an O3K
  database.
- `OpenStackSource` remains the source read boundary. Source control for
  quiesce and final synchronization is explicit and is not implicit source
  deletion.
- `run_opentofu_noop` invokes the external pinned tool as a process; the
  in-memory handoff proof is not used as a substitute.

## Evidence runner

`tests/p14_9a_real_two_cloud_acceptance.sh` emits a redacted,
source-commit-bound artifact and exits nonzero unless the real environment and
execution are available. `scripts/validate_p14_9a_evidence.py` requires all
twenty P14.9 gate rows and rejects manually promoted PASS results, secret-shaped
values, nonzero cleanup findings, and a missing process-level OpenTofu NO-OP.

The current repository has no provisioned source/destination environment, so
the runner must report `BLOCKED`; this artifact is not P14.9 acceptance
evidence and does not authorize closing #865 or #855.
