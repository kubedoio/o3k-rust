# ADR-0134 — Wire daemon compute requests through Placement

Status: accepted

The `o3kd` process opens its Placement ledger below the configured data
directory and constructs one scheduler from that ledger. When all compute-agent
control-plane TLS paths are configured, the compute service receives both the
scheduler and the authenticated `NodeRegistry`.

This keeps agent-backed daemon create requests aligned with the already-tested
compute service behavior: scheduled requests are constrained to registered,
available, administratively enabled agents, and Placement allocations survive
daemon restart through the configured data directory. An empty eligible-agent
set remains a fail-closed scheduling result. A local fake profile without the
agent control plane deliberately remains unscheduled so its portable workflow
does not require an external agent.

This decision wires the control-plane boundary only. It does not claim agent
inventory publication, libvirt artifact/network realization, guest lifecycle
success, or real-host acceptance; those remain the explicit follow-ups in
issues #78, #81, and #83.
