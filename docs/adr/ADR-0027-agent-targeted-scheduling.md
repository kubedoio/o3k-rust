# ADR-0027 — Agent-targeted scheduling contract

Status: Accepted as a scheduler/agent integration boundary.
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, placement, identity, governance

## Context

Placement scheduling selected any enabled provider with sufficient capacity,
but command dispatch needs an explicit identity match between the selected
provider and a registered compute agent. Falling back to another provider
after an agent-specific request could send work to the wrong host.

## Decision

`Scheduler::schedule_for_agent` restricts allocation to the exact requested
provider ID and returns `NoValidHost` instead of falling back. The scheduler
does not itself inspect `NodeRegistry`; a future control-plane bridge must
validate that the provider ID is the current authenticated agent identity and
epoch before dispatching a command.

## Consequences

Agent-targeted placement is now testable and fail-closed. Existing generic
first-fit scheduling remains unchanged. Full registry/epoch binding and real
dispatch remain follow-up integration work.
