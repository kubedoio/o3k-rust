# ADR-0023 — Stateful fake command realization

Status: Accepted as test-support coverage for the compute-agent command protocol.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, image, governance

## Context

The create command currently contains only logical image, flavor, and network
references. It cannot safely drive real libvirt because resolved artifacts,
network realization, config-drive material, and ownership manifests are not
part of the protocol. The previous executor rejected create commands entirely,
which left idempotency and failure cleanup untested through the command path.

## Decision

`FakeCommandExecutor` models a create as three owned stages—image, network, and
domain—with deterministic provider resource identity. It keys retries by
resource and payload fingerprint: equivalent duplicates return the original
result, while changed payloads fail closed. Injected stage failures clean
owned artifacts in reverse order, and delete removes the fake resource
idempotently. Inspect/start/stop/reboot operate only on resources owned by the
executor.

This executor is deliberately not used by the production libvirt binary and
does not claim host, guest, network, image, or restart evidence. A later real
implementation must add immutable artifact references, resolved execution
values, network/config-drive contracts, durable agent operation records, and
ownership manifests before dispatching create commands to libvirt.

## Consequences

The command/event protocol now has executable tests for duplicate delivery,
conflicting retries, staged failure cleanup, and absent-safe deletion. The
remaining real-realization work is explicit rather than hidden behind a fake
success path.
