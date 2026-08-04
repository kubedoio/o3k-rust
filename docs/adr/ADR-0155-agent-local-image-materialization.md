# ADR-0155 — Agent-local verified image materialization

Status: Proposed for the
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, image, identity, governance

#79 implementation boundary; human approval remains part of
#92.

## Decision

The compute agent resolves an image only from a committed authenticated
artifact manifest whose transfer, command, operation, resource, agent,
artifact, digest, format, and size identity all match. The agent then publishes
the bytes into its managed content-addressed image cache and creates the
instance-owned qcow2 overlay from that verified base.

Host paths remain agent-local implementation details. They are never placed in
protobuf messages, the control-plane store, or OpenStack responses. Overlay
ownership is persisted with the resource and agent identity; deletion is
identity-fenced and retains the shared digest-addressed base.

## Consequences

This makes transfer completion and image realization separate durable stages:
a committed artifact can be recovered and materialized idempotently after an
agent restart, while a mismatched or tampered manifest fails closed. The
network/TAP, config-drive, and full libvirt create orchestration remain later
service slices and are not claimed by this ADR.

## Provenance

The image data boundary follows the public Glance v2 image-content contract
(`PUT /v2/images/{image_id}/file`) and the repository contracts in
`SPEC-0005-glance-local-images.md` and `SPEC-0015-compute-agent.md`. The
normative public reference is the OpenStack Image Service API v2 documentation:
https://docs.openstack.org/api-ref/image/v2/index.html#image-data (accessed
2026-08-02).
