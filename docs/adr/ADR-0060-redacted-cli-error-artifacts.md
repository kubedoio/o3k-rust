# ADR-0060 — Do not persist raw CLI error output in evidence artifacts

## Status

Accepted for the OpenStack CLI harness.

## Context

The CLI workflow marks its JSON result as redacted, but it previously retained
raw token-authentication and console-query stderr files under the artifact
directory. Provider and client diagnostics can include endpoints, request
identifiers, or other sensitive response details.

## Decision

Authentication and console-query stderr are discarded at the command boundary.
The result artifact records only a generic, redacted reason and cleanup status;
the harness no longer creates raw error sidecars. Existing lifecycle outputs
remain bounded and resource identifiers only.

## Consequences

Evidence artifacts cannot accidentally publish raw CLI diagnostics. Operators
must reproduce a failed workflow with local command tracing when detailed
client errors are needed.
