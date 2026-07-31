# SPEC-0015 — `o3k-compute` agent and control-plane protocol

Status: Accepted contract draft for issue #37; runtime implementation pending
Version: `o3k.compute.v1` / wire revision 1

## 1. Scope and boundary

This specification defines the identity, enrollment, transport, lifecycle
command, operation, observation, liveness, and recovery contract between
`o3kd` and one `o3k-compute` process. The contract is accepted; runtime
implementation and real-host evidence are tracked separately by issue #38 and
the later release issues.

The execution path is:

```text
OpenStack client -> o3kd -> versioned mTLS stream -> o3k-compute
                                      -> local qemu:///system
```

`o3kd` owns OpenStack semantics, project authorization, resource IDs, desired
state, scheduling, operation state, and reconciliation decisions. The agent
owns bounded host-local execution, capability reporting, and observations.
The agent is the only component allowed to access the host system libvirt
socket. A remote libvirt URI, arbitrary URI, or user-supplied socket path is
not a valid protocol value. This issue does not implement libvirt, scheduling,
images, networking, DHCP, config-drive, or the agent binary.

The protocol contains no CellHV types, IDs, state names, RPCs, or payloads.
CellHV remains an optional provider behind the separate provider abstraction.

## 2. Package, transport, and message rules

The committed draft is `proto/compute/v1/compute_agent.proto`, package
`o3k.compute.v1`. It uses a bidirectional `Control` stream initiated by the
agent. `ControlRequest` is agent-to-control-plane and carries registration,
heartbeats, operation updates, observations, and acknowledgements.
`ControlResponse` is control-plane-to-agent and carries registration/heartbeat
responses, commands, desired administrative state, observation acknowledgements,
and resynchronization requests. Each stream begins with registration and then
carries typed envelopes. Protobuf unknown fields must be ignored and preserved
by compatible intermediaries; no field number may be reused. Removing a field
requires reserving its number and name.

Transport requirements:

- TLS 1.3 is the minimum operational version; the control-plane server
  authenticates the endpoint and the agent presents a client certificate.
- The certificate SAN/URI is bound to the registered `agent_id`; a valid CA
  chain alone is insufficient authorization.
- Messages and streams have bounded sizes. The implementation must reject
  oversized commands, observations, capabilities, and console-log responses.
- The protocol has no arbitrary file paths, shell commands, XML, libvirt URI,
  bridge name, TAP name, or credentials in its common messages.

## 3. Enrollment and identity

An operator creates an enrollment record for a host. The record contains an
opaque `agent_id`, an expiry, allowed protocol versions, and a one-time random
bootstrap token delivered out of band. The agent generates a key pair locally,
creates a CSR, and sends `EnrollRequest` over server-authenticated TLS. The
request includes the token, CSR, requested host label, and supported versions;
it never includes a private key. The response contains the assigned ID, signed
client certificate chain, CA certificate/fingerprint, expiry, selected version,
and a rotation deadline. The token is invalid immediately after one accepted
request and is never logged or persisted in plaintext.

The stable `agent_id` identifies a host registration, not a process. Every
operational connection also has a random `agent_epoch`. The control plane
rejects a certificate whose identity does not match the registered ID, and
rejects a second live epoch according to an explicit fencing policy: the newest
authorized registration may fence the old stream, while commands already
accepted by the old stream remain durable and must be reconciled.

Certificate rotation uses the `RotateCertificate` RPC over an authenticated
existing mTLS connection (or an operator-approved recovery path). The request
contains the stable `agent_id`, a newly generated CSR, and supported versions;
the private key never crosses the boundary. The response returns the new chain,
CA material, selected version, expiry, and next rotation deadline. Revocation
prevents new streams and command acceptance; it does not erase durable
operation records.

## 4. Version negotiation

`RegisterRequest.supported_versions` lists protocol package versions and wire
revisions the agent understands. The control plane selects one version and
returns it in `RegisterResponse`. No command is dispatched before selection.

Major-version changes require a new protobuf package and a new compatibility
review. Within major version 1, additive fields and enum values are allowed;
unknown enum values are treated as unsupported, not as a successful default.
Behavior changes that alter identity, operation, or state meaning require a new
wire revision or package. The agent must advertise capability names before the
control plane uses optional operations. Version overlap failure leaves the
agent unregistered and does not execute work.

## 5. Registration, heartbeat, and administrative state

Registration reports the stable identity, epoch, software version, host label,
protocol versions, and a complete typed `Capabilities` snapshot. The control
plane returns selected version, heartbeat interval, maximum clock skew, and
desired administrative state.

Heartbeats carry the epoch, monotonic sequence, send time, current admin state,
active operation count, and the highest observation sequence acknowledged by
the control plane. The control plane replies with its receive time, desired
state, and an acknowledgement sequence. Heartbeats are liveness only; they do
not prove that a mutation succeeded.

Administrative states are:

- `ENABLED`: accepts authorized commands supported by capabilities.
- `DRAINING`: rejects new create/start/reboot work with a retryable
  `AGENT_DRAINING` result, while accepted operations continue and observations
  are sent. It becomes quiescent when active operations reach zero.
- `DISABLED`: rejects new mutations. Observe, operation status, and cleanup
  reporting remain permitted so the control plane can recover state.

The control plane may request a transition with a desired-state envelope. The
agent durably applies the state and sends `AgentStateAck` containing the epoch,
applied state, transition sequence, and active operation count. Heartbeat
responses do not substitute for this acknowledgement. A disconnect leaves the
last accepted state in force until a newer fenced epoch is authorized.

Missing three consecutive heartbeats (or the configured lease interval) marks
the agent disconnected. The control plane does not automatically fail its
operations or dispatch duplicates. On reconnect, the agent first registers and
replays durable operation summaries and observations from the last acknowledged
sequence.

## 6. Capabilities

Capabilities are typed and bounded: host architecture, libvirt/QEMU feature
flags, vCPU and memory capacity, supported disk/image formats, lifecycle
actions, console-log support, and maximum console-log size. Capacity is an
observation, not an allocation; placement and allocation remain control-plane
concerns in later issues. Provider-native objects and CellHV models must not be
added as extensions to this common contract.

## 7. Commands and operations

Every command envelope has:

- `command_id`: unique delivery identity;
- `operation_id`: stable O3K mutation identity, globally unique and durable;
- `idempotency_key`: stable caller retry identity;
- `agent_id` and `agent_epoch` fencing values;
- O3K `resource_id` where applicable;
- a bounded command deadline and protocol version.
- `payload_fingerprint_sha256`, computed as lowercase hexadecimal SHA-256 over
  the canonical payload described below.

The mutation set is create, inspect, start, stop, reboot, delete, and
console-log retrieval. Create includes only an opaque O3K server ID, logical
resource references, and a `resolved` message with:

- immutable image artifact ID, SHA-256 digest, and `raw`/`qcow2` format;
- bounded vCPU, memory-MiB, and disk-GiB values;
- config-drive artifact ID and SHA-256 digest;
- network attachments containing port ID, MAC, and fixed IPv4 address.

References are bounded opaque identifiers; they cannot contain paths, shell
text, credentials, XML, or arbitrary URIs. The command builder rejects invalid
digests, formats, resource bounds, addresses, MACs, and duplicate ports before
computing the canonical fingerprint. Inspect and console-log are queries but
still carry request identity and authorization context; lifecycle mutations
always carry both operation identities.

The agent replies with `CommandAccepted` containing the same operation identity
and an agent-side operation record. Acceptance means only that the command was
durably recorded for execution. It does not mean libvirt accepted it.

Operation states are `ACCEPTED`, `RUNNING`, `SUCCEEDED`, `FAILED`, and
`UNKNOWN_OUTCOME`. Error categories distinguish invalid request, unauthorized,
conflict, capacity, not found, retryable, unknown outcome, and terminal error.
An accepted operation never moves backwards. A timeout after acceptance is
`UNKNOWN_OUTCOME` until a later observation determines the result.

## 8. Idempotency and durability

The control plane persists desired state and its operation record before
dispatch. The agent durably records `(agent_id, operation_id)` and the
idempotency-key fingerprint before acknowledging acceptance. The fingerprint
is lowercase hexadecimal SHA-256 of the deterministic protobuf wire
serialization of the committed `CanonicalCommandPayload` message. Its fields
are exactly `operation_id` (field 1), `resource_id` (field 2), and the action
oneof discriminator/payload in fields 3–9 (`create`, `inspect`, `start`,
`stop`, `reboot`, `delete`, `console_log`). Deterministic serialization uses
ascending field numbers, omits unknown fields, encodes absent proto3 scalar
values as their default, and preserves repeated attachment order. The legacy
`network_port_ids` field remains wire-compatible and is derived from the
validated attachment list by the command builder.
`command_id`, `idempotency_key`, `agent_epoch`, `deadline_unix_ms`,
`protocol_version`, and the fingerprint itself are excluded from the digest.
The control plane computes and sends the same digest; the agent rejects a
mismatch. Repeating the same operation and equivalent payload returns the
original operation/result without creating a second VM. Reusing an idempotency
key with a different operation, resource, or payload returns
`IDEMPOTENCY_CONFLICT` and performs no action.

The control plane and agent retain records for at least the configured retry
window and must not expire a record while the operation is non-terminal. After
expiry, a new key is required and reconciliation must first observe the
resource. Duplicate delivery is therefore safe, but idempotency is not a
substitute for authorization or state validation.

For create, delete, and every action: if the response is lost, the sender
reuses the same identities and asks for operation/resource observation before
retrying. It must never issue a fresh create merely because a command timed
out. An unknown delete is resolved by observing ownership and resource state;
foreign resources are never acted upon.

## 9. Observations and stale data

Observations contain the O3K resource ID, opaque agent resource ID, lifecycle
state, operation state, `observation_sequence`, agent epoch, observed time, and
bounded redacted diagnostics. Sequences increase monotonically per agent and
are persisted with the observation. The control plane acknowledges the highest
contiguous sequence it has committed.

An observation with an old epoch, a sequence no newer than the committed
sequence, or an impossible state transition is classified as stale by the
control plane and ignored (with an audit counter). Staleness is never
authoritatively declared by the sender. A gap causes buffering or
resynchronization, not speculative state advancement. The control plane treats a stale or missing observation as
unknown, not as proof of deletion or failure. After restart, the agent replays
unacknowledged observations and operation summaries; the control plane may
request a bounded resynchronization snapshot. This protocol does not define
domain discovery or ownership metadata; those belong to issue #40.

## 10. Console log

Console-log is a typed command scoped to an authorized O3K server ID. The
response contains bounded bytes, an offset, an end-of-log flag, and a
truncation flag. It cannot name a host path or request an arbitrary QEMU
channel. The agent may return `NOT_FOUND`, `UNSUPPORTED`, or a retryable
unknown outcome. Console output is potentially sensitive guest data: it is
access-controlled, size-limited, not written to control-plane logs, and not
included in generic error messages.

## 11. Failure, restart, and timeout rules

- **Agent restart:** stable identity and durable operation records survive;
  epoch changes; reconnect registration precedes replay; no duplicate command
  is sent.
- **Control-plane restart:** its journal reloads desired state and operation
  identities, fences stale streams if needed, and observes before retrying any
  unknown mutation.
- **Disconnect:** heartbeats stop and the agent is unavailable; operations are
  unknown unless a committed observation proves a terminal state.
- **Command timeout:** timeout is transport uncertainty, not failure. Use the
  same operation and idempotency identities to query/reconcile.
- **Duplicate delivery:** equivalent duplicate is acknowledged from the
  durable record; a mismatched duplicate is rejected.
- **Libvirt restart:** later issue-specific agent reconciliation reports local
  observations; this protocol does not claim libvirt recovery is implemented.
- **Certificate revocation:** reject new registration/commands and retain
  records for audit and reconciliation; never erase evidence to hide a stale
  outcome.

## 12. Threat model and security requirements

| Threat | Required control |
| --- | --- |
| Stolen enrollment token | One-use, short expiry, out-of-band delivery, audit, immediate invalidation |
| Stolen agent key/certificate | Private key never exported; SAN binding; rotation and revocation; least-privilege agent identity |
| Replay or duplicate command | TLS plus operation/idempotency identities, payload fingerprint, durable deduplication, epoch fencing |
| Unauthorized control-plane caller | `o3kd` authenticates and authorizes before dispatch; agent accepts only authorized stream commands |
| Compromised/stale agent | Certificate revocation, epoch fencing, capability/agent allow-list, bounded command set, audit |
| Malicious payload/path injection | Typed IDs and enums; no paths, XML, shell, or URI input; strict bounds and validation |
| Secret or guest-data disclosure | Never log credentials, keys, tokens, user-data, certificates, or console bytes; redact diagnostics and limit access |
| Resource exhaustion | Message, operation, log, capability, and replay limits; heartbeat backoff and bounded queues |
| Network interception | TLS 1.3, server authentication during enrollment, mutual authentication in operation |

The agent process must run with only the host privileges needed for its bounded
operations. It must not expose a general command runner or a remote libvirt
proxy. Security-sensitive certificate, privilege, ownership, and cleanup code
requires explicit human review.

## 13. Compatibility, tests, and non-goals

Compatibility is package-based: `o3k.compute.v1` is compatible only with an
overlapping supported wire revision and capability set. Additive protobuf
changes are allowed within the major version; semantic changes require a new
revision or package. CI validates the committed draft with `protoc`; future
agent work must add protocol, failure-path, restart, replay, and mTLS tests
before claiming implementation.

This issue does not implement the agent, libvirt adapter, qemu domain XML,
ownership discovery, image cache, scheduler, placement, bridge/TAP, DHCP,
config-drive, Nova wiring, CellHV models, or an OpenStack-facing API change.

## Public sources and provenance

- libvirt URI and architecture docs: <https://libvirt.org/uri.html> and
  <https://libvirt.org/architecture.html> (accessed 2026-07-31);
- gRPC authentication: <https://grpc.io/docs/guides/auth/> (accessed
  2026-07-31);
- TLS 1.3: RFC 8446, <https://www.rfc-editor.org/rfc/rfc8446> (accessed
  2026-07-31);
- O3K ADR-0005 and SPEC-0003;
- authored by an AI coding agent for issue #37; no private source, schema, or
  implementation was used.
