# ADR-0087 — Reject non-regular image-cache entries

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: image, governance

## Context

The image cache uses content-addressed base paths and deterministic overlay
paths. Files at those paths may be changed or pre-created outside the normal
publication flow. `Path::is_file` and ordinary reads follow symlinks, so a
symlink could make an outside file appear to be an owned cache entry. A
directory or another non-regular filesystem node could likewise be treated as
an existing overlay.

## Decision

Existing base and overlay destinations are inspected with `symlink_metadata`.
Only regular files are eligible for cache-hit validation or idempotent overlay
return. Symlinks, directories, and other non-regular nodes are rejected with
`InvalidPath`. Base ownership remains component-bound to the managed `base`
directory. New content is still written to a UUID-named temporary file and
published with an atomic rename; a conflicting non-regular destination is not
removed or followed.

## Consequences

Cache entries that were manually replaced with symlinks or special nodes fail
closed and must be repaired by an operator. The cache never reads or returns
an outside target through those entries. This is a repository-level safety
guard; it does not implement the missing Glance, compute-agent, or real-host
image lifecycle.

## Public sources

- Rust standard library `std::fs::symlink_metadata`, accessed 2026-07-31.
- O3K [image publication temporary policy](ADR-0035-image-publication-temporaries.md).
