# ADR-0038 — Honor bounded console-output requests

## Status

Accepted

## Context

The Nova console-output action already persisted bounded local output, but the
API ignored the request's `offset` and `length` fields and returned the entire
buffer. That made the wire behavior inconsistent with the bounded console
contract and could waste response bandwidth.

## Decision

Validate and pass `offset` and `length` to `ConsoleService::read_from`, using
offset zero and the service maximum as defaults. Authorization and the existing
empty-output behavior remain unchanged; the authenticated remote agent
transport is a separate follow-up.

## Consequences

Clients can page through stored output deterministically, while the service
still enforces its maximum buffer and read bounds. Guest-backed console
collection and agent command routing remain required for real host evidence.
