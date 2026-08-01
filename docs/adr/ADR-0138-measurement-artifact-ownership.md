# ADR-0138 — Serialize measurement artifact writers

Status: accepted

`tests/measure-testlab.sh` takes an exclusive `flock` on a lock file inside
the configured artifact directory before removing or writing any measurement
artifacts. A second run using the same directory fails without touching the
first run's raw samples, summary, diagnostics, or log.

This prevents concurrent control-plane measurements from mixing provenance
while retaining the existing stable `raw.json` and `summary.json` filenames
expected by the release gate. The lock is released by the operating system on
normal exit or process failure; it is not treated as evidence itself.
