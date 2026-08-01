# ADR-0146 — Publish authenticated agent inventory to Placement

## Status

Accepted for the repository-side issue #82 inventory slice.

## Context

`NodeRegistry` already retained authenticated agent capability snapshots, but
the daemon never projected those snapshots into its durable `PlacementLedger`.
Consequently an agent-backed scheduler had no provider inventory at runtime.
The protocol reports CPU and memory maxima but disk formats are not disk
capacity, and guessing capacity from a path would make scheduling unsound.

## Decision

`o3kd` starts a bounded periodic publisher when the authenticated compute-agent
control plane is enabled. It maps the stable agent ID to the Placement
provider ID and synchronizes VCPU, MEMORY_MB, and an explicit `max_disk_gb`
capability. Zero or missing capacity, unavailable agents, and disabled agents
are published as `Unavailable`; draining agents remain `Draining` and retain
existing allocations. `sync_provider` remains authoritative for usage and
preserves durable allocations across reconnects and restarts.

The new disk field is additive and defaults to zero. `o3k-compute` obtains it
from the operator's bounded `O3K_COMPUTE_MAX_DISK_GB` declaration. No host
filesystem probing, `disk_formats` inference, provider dispatch, or real-host
acceptance is claimed by this slice.

## Consequences

Registered agents now become visible to Placement and can be selected only
when all three required resource classes are explicitly available. Inventory
updates eventually converge within the publisher interval, while unknown
agent state fails closed. Agent-backed create/delete dispatch, restart
recovery, and real guest evidence remain issue #78 and the remaining #82
acceptance gates.
