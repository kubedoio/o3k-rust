# ADR-0092 — Fence libvirt console reads by domain ownership

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

The libvirt compute agent already verifies O3K ownership before lifecycle
mutations. Its console-log command, however, opened the domain console after
deriving a name from the command resource ID without inspecting the domain
metadata first. A stale, mismatched, or foreign domain must never become a
console source merely because its name is addressable.

## Decision

Before opening a libvirt console stream, inspect the derived domain and require
the same O3K ownership metadata and server-ID match used by lifecycle commands.
Reject missing, malformed, or mismatched metadata and do not open the stream.
The existing bounded snapshot, offset-zero restriction, and stream abort remain
unchanged.

## Consequences

Console reads fail closed for unowned or stale domains. This protects the
provider boundary without claiming that a real guest emits usable boot output;
guest, host, restart, and Nova end-to-end evidence remain issue #84 follow-ups.
