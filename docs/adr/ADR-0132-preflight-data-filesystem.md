# ADR-0132 — Measure the configured data filesystem during preflight

## Status

Accepted for the repository-side implementation of issue #89. This does not
claim a clean Ubuntu installation or real TestLab lifecycle.

## Decision

`packaging/preflight.sh` accepts `--data-dir` and runs its free-space check on
that path, or the nearest existing ancestor when the installation directory
has not been created yet. `packaging/install.sh` passes the validated data
directory into the libvirt preflight.

## Rationale

Checking a fixed `/var/lib` path is not evidence for an installation configured
to store state on another filesystem. The preflight must fail closed when that
actual filesystem has less than 1 GiB free or produces malformed `df` output.

## Verification

`tests/packaging-safety.sh` supplies a custom not-yet-created data directory to
a fake low-space `df`, verifies the selected existing ancestor was queried,
and requires preflight failure. The missing-`df` evidence regression remains.
