# ADR-0046 — Include an owned serial console device in domain XML

## Status

Accepted

## Context

The compute-agent protocol and console routing now support bounded console
queries, but the libvirt domain definition had no serial or console device.
Real guest boot output could therefore never reach a libvirt console stream.

## Decision

Every O3K-owned domain XML includes one PTY-backed ISA serial device and its
serial console target. The device is deterministic and part of the owned
domain definition; no arbitrary host path or guest-provided XML is accepted.

## Consequences

Future libvirt executor code can open the owned console stream after verifying
domain ownership. Actual stream draining, guest boot output, and host KVM
evidence remain separate acceptance work.
