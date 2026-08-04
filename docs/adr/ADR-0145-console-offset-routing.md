# ADR-0145 — Route nonzero console offsets to durable storage

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, storage, governance

## Context

The libvirt console stream is a non-seekable live snapshot. The compute-agent
command therefore supports only offset zero; historical offsets are served by
the durable console cache. The API previously attempted a live query for every
offset and relied on the error path for nonzero requests.

## Decision

The API queries a registered compute agent only when `offset == 0`. Requests
with a nonzero offset go directly to the bounded durable console cache. This
keeps paging deterministic and avoids dispatching commands the live PTY path
cannot satisfy.

## Consequences

The first request can refresh the cache from the live agent; subsequent or
offset requests read only persisted console evidence. Host acceptance and
actual guest-console output remain separate release gates.
