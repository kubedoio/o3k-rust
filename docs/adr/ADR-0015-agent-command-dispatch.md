# ADR-0015 — Agent command dispatch boundary

Status: Accepted for the alpha lifecycle slice.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, identity, governance

## Context

The compute-agent transport received authenticated commands but previously
discarded them. That made a healthy control stream indistinguishable from a
working compute host. The protocol already carries an action, resource ID,
operation ID, and fencing epoch, while the local libvirt adapter owns the
domain lifecycle calls.

## Decision

The agent validates command identity and epoch, emits a durable acceptance
event, invokes an injected `CommandExecutor`, and emits one redacted terminal
operation update. The `o3k-compute` binary supplies a libvirt executor for
inspect, start, stop, reboot, and delete. Domain names are derived from the
stable resource ID; the adapter remains responsible for libvirt ownership
checks.

Create and console-log commands fail explicitly until image resolution,
config-drive/network realization, and console transport are available in the
agent command path. The default library executor also rejects commands, which
keeps library users from accidentally claiming local resource ownership.

## Consequences

The control plane now observes command acceptance and bounded execution
results instead of silent drops. Resource creation and full host-backed
evidence remain open parts of issue #47 and must not be represented as
complete by the release gate.
