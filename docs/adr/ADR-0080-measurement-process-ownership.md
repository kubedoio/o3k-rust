# ADR-0080 — Measure only an owned control-plane process

Status: Accepted for the measurement and release-evidence boundary.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: network, identity, cli, governance

## Context

The measurement harness selected a loopback port and then started `o3kd`, but
did not prove that the port was free before the start. A pre-existing listener
could therefore make readiness succeed for an unrelated process. The harness
also continued from a readiness failure without a machine-readable reason
when its child exited, and it sampled token latency or RSS without rechecking
that the launched child still existed.

## Decision

Before launching `o3kd`, the harness probes the selected address and port and
fails with a redacted `diagnostic.json` when the port is occupied or otherwise
unavailable. During readiness polling, and immediately before token samples
and RSS collection, it requires the launched PID to remain alive. A child
exit at those checkpoints also fails with `diagnostic.json` identifying the
checkpoint and the harness log.

The port checker has an environment-injected command for deterministic tests;
normal measurements use a local socket bind probe. Tests use fake binaries and
fake port results and do not require libvirt, a host service, or a real
control-plane listener.

The existing libvirt prerequisite skip remains before the port preflight and
continues to produce the explicit `skipped` artifact. The existing exit-trap
cleanup remains responsible for stopping an owned child and removing the
temporary data directory.

## Consequences

Benchmark readiness, token, and RSS evidence can be attributed to the child
launched by the harness at the checked points. Occupied-port and child-exit
failures are diagnosable without exposing credentials or provider payloads.
The bind probe is a pre-launch check and cannot prevent a separate process
from racing for the port after the probe; the child-liveness checks and the
daemon log provide evidence for failures after launch.

## Non-goals

This decision does not claim guest, libvirt, or host performance evidence and
does not replace the real-host release gate.
