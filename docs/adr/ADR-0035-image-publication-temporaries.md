# ADR-0035 — Isolate all image publication temporary files

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: image, governance

## Context

The image cache already published overlays atomically, but base-image caching,
content uploads, and metadata persistence still used process-ID temporary
names. Concurrent writers could collide, and failed writes could leave stale
temporary files.

## Decision

Use UUIDv7 temporary names for base, overlay, content-upload, and metadata
publication. Remove the temporary path on both write and rename failures, then
publish only through the final atomic rename. Existing startup cleanup remains
compatible with the managed temporary naming convention.

## Consequences

Image publication no longer shares a temporary pathname between writers, and
failed writes do not leave a stale candidate for later recovery code to treat
as current data. Image cache locking, qemu-img host execution, and guest image
evidence remain otherwise unchanged.
