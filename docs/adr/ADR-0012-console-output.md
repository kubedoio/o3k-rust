# ADR-0012: Bounded durable console output

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, storage, governance

## Decision

Console output is stored under the operator-selected O3K data directory in a
file named from the server UUID. Writes replace files atomically; appends
retain only the newest 64 KiB. Nova `os-getConsoleOutput` first performs the
existing project-scoped server lookup and then returns the bounded content.

## Rationale

UUID-derived paths prevent tenant input from redirecting storage, while a
bounded tail avoids control-plane memory and disk exhaustion. Missing output
is an empty successful response because a guest may not have booted yet.

## Consequences

Output survives control-plane restart and is removed by explicit lifecycle
cleanup. Serial-console attachment to libvirt domain XML and compute-agent
streaming remain deployment/TestLab follow-ups; this service is the durable
retrieval boundary.
