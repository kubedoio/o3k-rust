# ADR-0137 — Fence the default SBOM output root

Status: accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: governance

When `packaging/make-sbom.sh` is invoked without an explicit output path, its
default `dist` root must be a real directory rather than a symlink or special
file. `O3K_RELEASE_DIST_DIR` provides an isolated default root for packaging
tests and controlled callers. Explicit output paths remain caller-selected and
are created only at their requested parent.

This keeps standalone SBOM generation aligned with the release builder's
output-root fence and prevents an accidental symlink from redirecting default
release metadata outside the checkout. It does not claim release readiness or
publication.
