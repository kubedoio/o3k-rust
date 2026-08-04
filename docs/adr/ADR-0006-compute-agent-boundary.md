# ADR-0006 — `o3k-compute` agent boundary and protocol

Status: Accepted
Date: 2026-08-01
Supersedes: none
Superseded-by: none
Affected-services: compute, network, governance

## Context

ADR-0005 makes libvirt/KVM the primary real compute backend and prohibits the
control plane from opening a libvirt connection. A compute host therefore needs
a small, separately privilegeable agent. Issue #37 requires the contract before
the agent or any libvirt implementation is built.

The boundary must support reconnectable long-running operations without making
a lost response look like a failed operation. It must also provide a clear
identity and authorization boundary for a process that can perform privileged
host actions.

## Decision

Define a versioned protobuf/gRPC contract in package `o3k.compute.v1`, with the
following two phases:

1. **Enrollment:** a one-time, short-lived bootstrap credential is presented
   over a TLS connection that authenticates the control-plane server. The agent
   creates its private key locally and submits a CSR; the private key never
   crosses the boundary. The control plane binds the issued certificate to the
   assigned `agent_id` and host identity.
2. **Operation:** the agent connects to `o3kd` using mutual TLS. The certificate
   identity is authorized for exactly one agent record and is required for
   registration, heartbeats, commands, and observations.

The operational service is a bidirectional control stream. The control plane
can send registration responses, desired agent state, and commands; the agent
returns command acknowledgements, durable operation updates, and observations.
Unary status and console-log RPCs are intentionally not added: one ordered,
reconnectable stream gives the control plane a single ownership and liveness
model. The exact wire draft is committed at
`proto/compute/v1/compute_agent.proto`.

The control plane remains authoritative for OpenStack resource identity,
authorization, desired state, scheduling, and operation state. The agent owns
only host-local execution and provider observations. The only permitted
libvirt target is `qemu:///system` on the compute host running the agent.

## Alternatives rejected

- **Direct `o3kd` → libvirt:** violates ADR-0005, expands control-plane
  privileges, and makes remote-socket access too easy to configure.
- **An unauthenticated polling API:** cannot safely authorize commands or
  prevent a stale host from acting after certificate revocation.
- **CellHV-shaped messages:** couples the release-critical contract to an
  optional provider and leaks provider-specific models into the common
  boundary.
- **Ephemeral request/response operations:** a timeout or reconnect would lose
  the operation identity and could duplicate a VM. Operations are durable and
  observed separately from command delivery.

## Consequences

- The agent can be restarted without changing its stable `agent_id`; its
  `agent_epoch` changes and outstanding operation identities remain durable.
- A disconnected or timed-out command is `UNKNOWN_OUTCOME` until observation
  proves a terminal result. Retries reuse the same identities.
- Disable and drain are explicit control-plane states. Draining stops new
  create/boot work but allows already accepted operations to reach a terminal
  state or be reported unknown.
- Certificate issuance, rotation, revocation, and agent authorization are
  security-sensitive operations requiring human review and audit evidence.
- This ADR does not authorize implementation of the agent, libvirt adapter,
  scheduler, image transfer, networking, or CellHV support.

## Security decision

The control plane must validate the client certificate chain, expiry, key usage,
and SAN/URI binding to the registered `agent_id` before accepting a stream. An
enrollment token is single-use, scoped to one host registration, expires
quickly, and is never written to logs. Commands are authorized by the control
plane before dispatch; the agent rejects commands for another `agent_id` or
malformed resource/operation identity. Payloads are bounded and diagnostic
details are redacted. Private keys, bootstrap tokens, certificates, user-data,
and guest credentials are never logged.

## Acceptance review

The issue-#37 acceptance review confirms that the committed protobuf defines
registration, heartbeat, capabilities, administrative state, lifecycle and
console-log commands, durable operation identities, observations, replay, and
resynchronization. The specification documents restart, timeout, duplicate
delivery, version skew, remote-libvirt prohibition, and certificate,
authorization, replay, injection, disclosure, and resource-exhaustion
controls. Descriptor generation, workspace tests, clippy, and protobuf
compatibility validation are CI gates.

This acceptance applies to the contract and architecture decision only. It
does not claim that certificate enrollment, the agent runtime, libvirt
execution, or real-host security evidence has been implemented; those are
covered by issues #38 onward and remain release-gate evidence requirements.

## Public references and provenance

- libvirt URI documentation: <https://libvirt.org/uri.html> (accessed
  2026-07-31);
- libvirt architecture documentation: <https://libvirt.org/architecture.html>
  (accessed 2026-07-31);
- gRPC authentication guidance: <https://grpc.io/docs/guides/auth/> (accessed
  2026-07-31);
- TLS 1.3: RFC 8446, <https://www.rfc-editor.org/rfc/rfc8446> (accessed
  2026-07-31);
- O3K decisions: ADR-0005 and SPEC-0003. No private implementation or schema
  was used.
