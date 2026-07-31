# ADR-0093 — Verify absence of every CLI-owned resource

## Status

Accepted

## Context

The OpenStack CLI acceptance harness tracked the image, network, subnet,
flavor, and server IDs it created. It verified that the server disappeared,
but treated a successful delete exit status for the dependent resources as
proof of cleanup. A provider or CLI no-op could therefore leave a managed
resource behind while the redacted artifact reported `cleanup: passed`.

## Decision

After each dependent-resource delete, the harness performs a public CLI
`show` and accepts absence only when the command fails with a recognizable
not-found response. The same observation is used by failure cleanup. Delete
errors are treated as unknown outcomes and are followed by observation, so a
resource that disappeared despite a timeout is not incorrectly reported as a
leak. IDs are retained when absence is not proven, allowing the exit handler
to retry the owned cleanup and report failure honestly.

## Consequences

- Image, network, subnet, flavor, and server cleanup now has observable
  absence evidence.
- Successful delete commands that do nothing fail the workflow.
- The check remains public-API-only and does not claim a real OpenStack or
  CirrOS run; trusted host evidence remains required by issue #86.
- A provider-specific not-found message not covered by the bounded matcher
  is treated as an unknown outcome and must be added with a regression test
  before acceptance.

## Provenance

This is an independently authored shell workflow decision based on the
public OpenStack CLI resource `show` contract and the repository's redacted
evidence and unknown-outcome rules. No private implementation or test was
used.
