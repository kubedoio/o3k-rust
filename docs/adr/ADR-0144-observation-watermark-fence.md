# ADR-0144 — Persist agent observation watermarks

Status: accepted

Successful agent observations are applied through an atomic SQLite transaction
that updates the resource projection and a durable `(agent_epoch,
observation_sequence)` watermark. A replay or lower sequence from the same
epoch is idempotently ignored, so delayed observations cannot regress Nova
state after a newer observation has been committed.

The live compute event consumer also compares each observation epoch with the
currently registered agent epoch before applying it. A replaced stream cannot
publish an old epoch into the projection. A new epoch may establish a new
watermark after the registry has accepted the replacement connection.

This is the repository-side ordering boundary for issue #83; real lifecycle
dispatch, restart reconciliation, and guest evidence remain host-gated.
