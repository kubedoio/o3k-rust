# ADR-0059 — Ownership fencing for libvirt agent mutations

## Status

Accepted for the compute-agent lifecycle boundary.

## Context

The local compute executor derived a stable domain name from a command resource
ID and then inspected or mutated that name. A foreign or malformed domain with
the same derived name could therefore be exposed or changed.

## Decision

Before inspect, start, stop, reboot, or delete, the executor parses the domain
metadata and requires an O3K-owned domain whose `server_id` exactly matches the
command resource ID. Failure is generic and fail-closed. Create orchestration
and agent command dispatch remain separate follow-up work.

## Consequences

Lifecycle operations cannot mutate a foreign same-name domain. Existing
provider error redaction remains intact, while ownership metadata becomes a
mandatory precondition for host mutation.
