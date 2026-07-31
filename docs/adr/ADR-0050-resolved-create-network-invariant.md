# ADR-0050 — Require a network attachment in resolved create commands

## Status

Accepted

## Context

The resolved compute-agent create contract validates each network attachment,
but previously accepted an empty attachment list. Nova server creation in this
alpha requires at least one network, and allowing an empty list would let an
invalid command reach a future host realization path.

## Decision

Reject empty network attachment lists in both the public create-command builder
and the fake executor's resolved-input validator. The check is independent of
the API layer so commands remain valid when they are retried or received after
a control-plane restart.

## Consequences

Malformed create commands fail before any host artifact or domain mutation.
Full production create dispatch still requires the separate resolved-artifact,
agent-backed provider, and real-host integration work tracked under issue #47.
