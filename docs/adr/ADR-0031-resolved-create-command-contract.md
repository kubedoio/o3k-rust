# ADR-0031 — Typed resolved inputs for agent create commands

Status: Accepted as the create-contract slice for issue
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, image, governance

#47.

## Context

The original compute-agent create payload contained only logical image,
flavor, and port identifiers. An agent could not validate immutable artifacts
or execution values, while sending host paths or provider XML would cross the
control-plane/host boundary and make ownership difficult to enforce.

## Decision

Extend `CreateCommand` with a typed `ResolvedCreateInputs` message containing
an opaque image artifact reference, SHA-256 digest, image format, bounded
vCPU/memory/disk values, a config-drive artifact reference and digest, and
network attachments containing port ID, MAC, and fixed IPv4 address. The
builder validates reference characters, digest shape, supported formats,
resource bounds, MAC/IPv4 syntax, and duplicate ports before computing the
canonical fingerprint. Legacy logical fields remain wire-compatible;
`network_port_ids` is derived from validated attachments by the builder.

Paths, XML, shell commands, credentials, and arbitrary URIs are not valid
references. This contract does not dispatch creates or claim that an agent can
yet realize the artifacts on a real libvirt host.

## Consequences

The agent boundary now carries enough typed information for a later resolver
and host realization implementation to validate inputs deterministically, and
changes to any resolved input change the command fingerprint. The real create
executor, artifact transfer/ownership, and integration evidence remain
follow-up work.
