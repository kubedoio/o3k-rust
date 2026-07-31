# ADR-0114 — Emit observations for every successful agent command

## Status

Accepted for the issue #78 protocol evidence-completeness slice.

## Context

The compute agent emitted the protocol `Observation` message only when a
successful executor result contained console bytes. Successful lifecycle and
inspection results therefore had operation, resource, provider, and state
information only in the separate operation update, leaving the observation
stream incomplete for protocol consumers.

## Decision

For every successful `CommandExecutor` result, emit one `Observation`. Preserve
console bytes and metadata when the result includes them; otherwise use the
protocol defaults (empty bytes, offset zero, and false completion/truncation
flags). Keep the existing operation/resource/provider/state fields populated
from the command and result.

## Evidence boundary and non-goals

This is evidence completeness for the compute-agent protocol only. It does not
claim that the full `o3kd` lifecycle adapter, durable lifecycle convergence, or
real mTLS command execution is implemented or accepted. Real libvirt, guest,
host, and protected mTLS evidence remain separately gated.

## Consequences

Protocol consumers receive a consistent observation for successful Inspect,
Start, Stop, Reboot, Create, Delete, and console-log executor results. The
existing protobuf contract is sufficient; no wire change is needed.
