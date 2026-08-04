# ADR-0141 — Fence events from replaced agent streams

Status: accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: placement, identity, governance

The control-plane registry treats the connection epoch currently attached to an
agent identity as authoritative. Before publishing operation updates,
observations, command acknowledgements, or protocol errors, the stream handler
must still own that epoch. Registering a replacement connection invalidates
the prior stream, so late messages from it cannot update the live event bus.

This closes a repository protocol-safety gap for issue #78. It does not claim
durable command replay or production lifecycle dispatch through the agent;
those remain separate implementation and real-host acceptance requirements.
