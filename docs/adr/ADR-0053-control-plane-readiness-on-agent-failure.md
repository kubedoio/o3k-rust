# ADR-0053: Reflect compute control-plane startup failure in readiness

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, governance

## Context

`o3kd` starts its mTLS compute-agent listener in a background task so the HTTP
API can serve health endpoints. Previously, a TLS-material parse failure or a
control-address bind failure ended that task while the API remained ready
because readiness only reflected the selected provider probe. Operators and
packaging checks could therefore see a ready daemon with no secure agent
control plane.

## Decision

When the configured compute control-plane task returns an error before normal
shutdown, it atomically clears the shared `o3kd` readiness flag and logs only
the typed, redacted error. The API remains alive so `/readyz` exposes the
failure and `/healthz` remains useful for diagnosis. An intentionally disabled
control plane does not affect provider-only readiness. Existing configuration
validation still rejects partial TLS settings before listeners are opened.

## Consequences

Startup and runtime control-plane failures become machine-visible without
leaking certificate material or terminating the public API process. The
control-plane task is still process-local and node state is not yet durable;
restart persistence remains a separate follow-up.
