# ADR-0119 — Propagate explicit resource state in agent observations

## Status

Accepted for the repository protocol/agent slice of issue #83.

## Context

Successful command observations carried operation state and identity but left
the resource-state field unspecified. Consumers therefore could not distinguish
a running, stopped, deleted, or error projection from the observation stream
even when the executor had observed the state after a command.

## Decision

Require `CommandExecutionResult` to carry a provider resource state. Populate
it in the fake executor and in the libvirt compute executor by inspecting the
domain after lifecycle commands; deletion reports `Deleted`. Copy that value
into every successful protocol `Observation`, including console observations.

## Evidence boundary and non-goals

This proves only repository propagation and portable fake/libvirt command
mapping. It does not claim a real agent-backed lifecycle, a running guest,
restart reconciliation, or real-host evidence.

## Consequences

Protocol consumers receive an explicit resource-state projection for successful
commands, while failed commands retain the existing operation error path.
Libvirt lifecycle commands perform a post-command inspection before reporting
success, so an observation reflects observed state rather than command intent.
