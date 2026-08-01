# ADR-0047 — Provide bounded libvirt console stream reads

## Status

Accepted

## Context

Owned domains now expose a PTY-backed serial console, but the production
compute agent still rejected console commands. The libvirt binding provides a
domain console stream that can be sampled without exposing a host path.

## Decision

The libvirt adapter opens the local system domain console with a nonblocking
stream, reads at most 64 KiB, and aborts the stream after the bounded snapshot.
The production agent exposes this as a console observation. Offset queries are
rejected by the production snapshot source because a live PTY is not a
seekable historical log; the persisted daemon cache remains responsible for
sequential paging.

Authenticated API reads prime that durable cache and serve the response from
it. If an agent is registered but its control stream is unavailable, or if a
live observation is empty or stale, the API falls back to the persisted cache
with the requested offset and bound. It does not report an unpersisted live
snapshot as durable console evidence.

## Consequences

Real agents can now return bounded current console bytes when the host and
guest provide the configured console. Empty snapshots are valid. Actual guest
boot output, stream behavior, and cleanup still require the real-libvirt host
harness.
