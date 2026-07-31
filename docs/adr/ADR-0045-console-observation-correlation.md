# ADR-0045 — Correlate console observations to the fenced agent

## Status

Accepted

## Context

The console query wait path matched resource and operation IDs, but the event
bus can carry observations from multiple authenticated agents. Correlation
must retain the command's identity and epoch fence as well as its resource and
operation identifiers.

## Decision

Accept an observation only when agent ID, agent epoch, resource ID, and
operation ID all match the dispatched command. Other observations remain
available to their own consumers.

## Consequences

A stale or cross-agent observation cannot satisfy a console request. The
existing registry dispatch fence and bounded timeout remain unchanged.
