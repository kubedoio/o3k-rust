# ADR-0066 — Select the configured CellHV provider

## Context

The daemon accepted `provider = "cellhv"` but constructed the fake provider
for that profile. This silently reported fake capabilities and sent no
requests to CellHV.

## Decision

Require a CellHV endpoint and expected protocol version in configuration, then
construct `CellHvProvider` with the configured TLS material and connect before
the daemon starts serving requests. Fake, CellHV, and libvirt profiles remain
distinct.

## Consequences

An unavailable or misconfigured CellHV endpoint fails startup instead of
degrading to fake compute. HTTPS deployments must provide the CA, client
certificate, and client key through the existing configuration precedence.
Live CellHV acceptance remains environment-dependent.
