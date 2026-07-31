# ADR-0062 — Revalidate content-addressed image-cache hits

## Context

`ImageCache::cache_base` names cached files by checksum, but a file can be
corrupted or replaced after publication. Returning an existing path without
reading it would allow an invalid base image to be used as verified content.

## Decision

On every cache hit, read the file and verify its size and SHA-256 checksum
before returning it. If validation fails, remove the entry and republish the
already-verified upload atomically.

## Consequences

Cache hits perform a bounded integrity read, and invalid entries are repaired
without changing the content-addressed path. A storage error remains visible
to the caller instead of being treated as a cache miss.
