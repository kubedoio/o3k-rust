# O3K execution-boundary contract

Status: Proposed

Related decisions:

- [ADR-0160](../docs/adr/ADR-0160-service-topology-and-execution-boundaries.md)
- [ADR-0161](../docs/adr/ADR-0161-keystone-trust-and-service-identity.md)
- [SPEC-0021](../docs/specs/SPEC-0021-cross-service-workflows-and-compensation.md)

## Purpose

This document defines the public architectural contract between `o3kd` and the
host-local execution capabilities represented by:

- `o3k-compute`;
- `o3k-network`;
- `o3k-storage`.

The current protobuf schema may implement only a subset. New wire fields or
actions must preserve these authority, identity, security, retry, and evidence
rules.

## Authority boundary

### `o3kd` is authoritative for

- user, project, service, and policy identity;
- public OpenStack resource IDs and representations;
- desired state;
- immutable request snapshots;
- operation identity and workflow phase;
- scheduling and Placement allocation;
- retry, compensation, and reconciliation decisions;
- compatibility behavior and public errors.

### Execution agents are authoritative only for

- their current capabilities and health;
- provider-native resource IDs;
- whether an owned provider resource exists;
- provider-native observed state;
- bounded local artifact and host-resource state;
- redacted provider failure classification.

An agent cannot authorize a project, select a tenant, allocate control-plane
capacity, or create a new public O3K resource identity.

## Common command envelope

Every mutating or inspecting command includes:

```text
protocol_version
command_id
operation_id
idempotency_key
resource_id
resource_generation
project_id or opaque authorization binding
target_agent_id
target_agent_epoch
deadline
trace_id / request_id
action
canonical_payload_fingerprint
```

Requirements:

- identities are bounded typed values;
- `command_id` is deterministic where replay is required;
- the payload fingerprint is derived from canonical typed content;
- a command is accepted only by the selected current agent epoch;
- deadline expiry prevents new mutation but does not erase a previously
  accepted outcome;
- the agent persists acceptance before external mutation;
- the same identity with different payload is a conflict;
- a duplicate equivalent command replays durable status rather than mutating
  again.

Raw user tokens, passwords, private keys, service credentials, and unrestricted
provider payloads are forbidden in the envelope.

## Common response and event types

The protocol distinguishes:

- registration result;
- heartbeat and administrative-state acknowledgement;
- command accepted;
- operation update;
- observation;
- artifact acknowledgement/status;
- resync request;
- typed protocol error.

Command acceptance is not resource success. Operation completion is not
accepted unless its identity and generation match the durable operation.

## Observation contract

An observation includes:

```text
agent_id
agent_epoch
resource_id
provider_resource_id
operation_id
resource_generation
observation_sequence
observed_at
operation_state
resource_state
error_category
redacted_message
optional bounded typed evidence
```

Requirements:

- sequence is monotonic within an agent epoch;
- stale epochs and stale generations are rejected;
- provider-resource identity cannot change silently;
- unknown outcome is preserved until observation resolves it;
- observations cannot create an unknown control-plane resource;
- observations cannot change project ownership;
- console or diagnostic bytes are bounded and separately authorized;
- provider paths are never returned as public API fields.

## Capability contract

Capabilities are typed, versioned, and bounded. They include only facts needed
for scheduling or compatibility decisions.

### Compute examples

- architecture;
- vCPU and memory capacity;
- local disk capacity;
- supported machine, firmware, disk, console, and lifecycle features;
- artifact-transfer support;
- config-drive support;
- libvirt/QEMU versions where safe to report.

### Network examples

- flat, VLAN, VXLAN, bridge, OVS, or OVN modes;
- IPv4/IPv6 and DHCP support;
- MTU bounds;
- routing, NAT, and policy features;
- supported port-binding modes.

### Storage examples

- backend type;
- capacity and allocation unit;
- volume, snapshot, clone, encryption, and attachment capabilities;
- supported connection modes;
- backend availability zone or failure domain.

A capability is not support evidence until the conformance suite and required
real-host gate pass.

## Compute actions

The first compute contract may include:

- create instance;
- inspect instance;
- start instance;
- stop instance;
- hard reboot instance;
- delete instance;
- read bounded console output;
- query committed artifact status;
- reconcile owned resources.

A create command references immutable, already-authorized inputs:

- server ID and project binding;
- flavor snapshot;
- image content identity, digest, size, and format;
- config-drive identity and digest;
- typed network attachments;
- expected ownership metadata;
- resource limits and deadline.

It must not contain a control-plane filesystem path.

Compute-owned state includes only O3K-owned:

- domain metadata and provider ID;
- image/base references and instance overlay;
- config-drive;
- console artifact;
- compute-local journal and manifests.

## Network actions

The network contract is logically independent even when its executor is hosted
inside `o3k-compute`.

The first network contract may include:

- realize port binding;
- inspect port binding;
- remove port binding;
- configure/reconcile DHCP state;
- inspect connectivity evidence;
- reconcile owned links and leases.

A binding command references:

- network, subnet, and port IDs;
- project binding;
- selected host;
- MAC;
- fixed IP;
- CIDR, gateway, allocation context, DNS, and MTU as required;
- provider-network identity;
- security-policy reference when the selected profile supports it.

Network-owned state includes only O3K-owned:

- TAP or equivalent interface;
- bridge/OVS/OVN binding record;
- DHCP host/lease/config fragment;
- routing/NAT/policy state in supported profiles;
- network-local journal and manifests.

The network executor cannot allocate a different public fixed IP or MAC without
an accepted control-plane operation.

## Storage actions

The storage contract is activated by the persistent-volume profile.

The first storage contract may include:

- create volume;
- inspect volume;
- delete volume;
- prepare attachment;
- terminate attachment;
- create/delete snapshot where later declared;
- reconcile owned backend resources.

A volume command references:

- volume ID and project binding;
- immutable size and volume-type snapshot;
- selected backend and availability/failure domain;
- encryption reference when supported;
- attachment identity and selected compute host where applicable.

Storage-owned state includes only O3K-owned:

- backend volume/provider ID;
- attachment/session preparation;
- backend-local journal and ownership metadata.

Connection information is treated as secret-bearing operational data. It is
never logged or uploaded as ordinary CI evidence and is returned only through a
bounded authenticated path.

## Artifact transfer

Artifact transfer uses:

- deterministic transfer ID;
- command, operation, resource, and agent identity;
- artifact kind;
- immutable artifact ID;
- byte length;
- digest;
- bounded chunk size/count;
- expiry;
- accepted/receiving/committed/rejected state.

Requirements:

- bytes are authenticated and digest-verified;
- partial content is never trusted as committed;
- publication is atomic;
- expired offers cannot begin new transfer;
- committed artifacts required for cleanup remain identifiable after the
  original offer expires;
- symlinks, special files, path traversal, and user-selected host paths are
  rejected;
- cleanup is ownership-checked and reference-aware.

## Error model

Errors use stable categories rather than unbounded provider text:

- invalid request;
- authorization binding failure;
- unsupported capability;
- conflict;
- not found;
- capacity exhausted;
- transient unavailable;
- timeout;
- unknown outcome;
- terminal provider failure;
- ownership ambiguity;
- internal protocol failure.

The redacted message is bounded and contains no secret, raw command, provider
connection information, arbitrary XML, or filesystem path unless a separately
protected operator artifact explicitly allows a safe relative identifier.

## Security and privilege rules

- all network connections use mutually authenticated transport;
- certificate identity binds to the agent ID;
- old agent epochs are fenced;
- each agent runs as a dedicated non-root account;
- capabilities are minimal and documented per action;
- compute, network, and storage privilege sets are reviewed independently;
- an agent rejects commands outside its activated capability domain;
- foreign resources are never mutated or deleted;
- ambiguous ownership fails closed;
- destructive cleanup requires deterministic ownership evidence.

## Reconnect and replay

On connection loss:

- the agent remains alive unless a local fatal error or shutdown occurs;
- readiness becomes false while disconnected;
- reconnect uses bounded exponential backoff;
- a new epoch is registered;
- accepted/running local journal entries become unknown outcome unless a
  terminal result was durably recorded;
- terminal results and artifact status may be replayed idempotently;
- the control plane observes before redispatching mutation.

On control-plane restart:

- agent connections re-register;
- control-plane operations are loaded before reconciliation;
- no new provider identity is invented;
- stale observations are rejected;
- selected host/backend and allocations remain durable.

## Conformance suites

Every provider implementation passes a shared suite appropriate to its domain.

### Common

- registration and capability validation;
- mTLS identity and epoch fencing;
- command identity/fingerprint conflict;
- duplicate replay;
- timeout and unknown outcome;
- restart and reconnect;
- stale observation;
- bounded errors and secret redaction;
- ownership and foreign-state protection.

### Compute

- create/inspect/lifecycle/delete;
- image/config-drive identity;
- console bounds;
- no duplicate domain;
- cleanup and restart discovery.

### Network

- realize/inspect/remove binding;
- deterministic MAC/IP identity;
- DHCP/binding reconciliation;
- no duplicate link or lease;
- cleanup and foreign-link protection.

### Storage

- create/inspect/delete volume;
- attachment preparation/termination;
- secret-safe connection data;
- no duplicate backend volume;
- cleanup and foreign-volume protection.

## Versioning

- protocol major changes require an ADR and incompatible-version rejection;
- additive optional fields require explicit default and downgrade semantics;
- action behavior changes require contract fixtures and compatibility review;
- independent agent releases declare supported protocol ranges;
- a process split cannot change OpenStack public behavior without a separate
  public-contract decision.
