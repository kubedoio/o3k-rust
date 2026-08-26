# ADR-0177 — Canonical NetworkPolicy and reusable policy-set lifecycle

Status: Proposed
Date: 2026-08-26
Supersedes: none
Superseded-by: none
Affected-services: kernel, network, store, api, compute, compatibility, governance

Related decisions and specifications:

- [ADR-0165 — O3K Cloud OS and Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0168 — O3K Routed Fabric and node-local network execution](ADR-0168-o3k-routed-fabric-and-network-execution.md)
- [ADR-0171 — AddressRealm-encapsulated edge fabric](ADR-0171-addressrealm-encapsulated-edge-fabric.md)
- [SPEC-0026 — O3K Routed Fabric v1](../specs/SPEC-0026-o3k-routed-fabric-v1.md)
- [SPEC-0034 — Canonical NetworkPolicy lifecycle v1](../specs/SPEC-0034-canonical-networkpolicy-lifecycle-v1.md)
- [P13.3A provider discovery](../compatibility/p13-3/p13-3a-security-group-provider-contract.json)

This is an architecture proposal. It is not an acceptance of a new canonical
resource, does not authorize runtime implementation, and does not advertise
Neutron Security Group compatibility.

## Context

P13.3A discovery confirmed that a Neutron Security Group is an independent,
project-owned object which can exist before it is attached to a Port. A rule
also has independent create/read/delete/import identity. The current O3K
policy path cannot represent either fact cleanly: `PolicyIntent` requires an
`endpoint_id`, and `canonical_network_policies` has an endpoint foreign key.
Existing security-group records are compatibility projection state and must
not become an alternative cloud authority.

The current Linux policy provider is a bounded stateful execution provider. It
accepts endpoint-specific intents, accepts established/related conntrack
traffic, and installs an explicit host policy. This is useful legacy execution
behavior, but it is not a reusable policy lifecycle or proof that Neutron's
allow-list defaults match O3K defaults.

## Decision proposal

Adopt Option B/D: an independent canonical `NetworkPolicy`, independently
identifiable `NetworkPolicyRule` resources, and durable `PolicyAttachment`
relations. Keep `PolicyIntent` as a derived endpoint-targeted execution input.
The model is provider-independent and remains meaningful to native O3K clients.
A later accepted profile may project Neutron Security Groups and Rules onto it.

## Options evaluated

### Option A — extend `PolicyIntent`

Rejected. An optional endpoint would mix reusable desired state with compiled
realization, leaving rule identity and attachment lifecycle ambiguous.

### Option B — reusable policy plus attachments

Selected as the aggregate shape. A detached policy is valid and one policy may
attach to many endpoints.

### Option C — value-only embedded rules

Rejected for the v1 lifecycle. A rule array index, order, or content hash
cannot safely preserve an independent provider resource identity through
duplicates, deletion, and import.

### Option D — independent policy and rule resources

Selected with Option B. A rule has a durable UUID and parent policy relation;
attachments remain separate relations.

## Proposed canonical model

```text
NetworkPolicy { id, project_id, name, description, state, generation,
                stateful_mode, unmatched_action, created_at, updated_at }
NetworkPolicyRule { id, policy_id, project_id, direction, address_family,
                    protocol, port_range, remote_selector, action, state,
                    generation }
PolicyAttachment { id, policy_id, endpoint_id, project_id, state, generation }
```

Policies, rules, and attachments use canonical UUIDs. A policy is project-owned
and may be unattached. A rule belongs to exactly one policy and project. An
attachment requires active same-project policy and Endpoint. The domain does
not impose Neutron's attachment-count limit. An attachment UUID is retained
for lifecycle, audit, generation, and conflict identity even if its natural
uniqueness is `(policy_id, endpoint_id)`.

Rules use typed direction (ingress/egress), address family (IPv4 first),
protocol (any/TCP/UDP/ICMP), optional inclusive ports, typed remote selectors,
and Allow/Deny action. Rule UUID, not order or provider handle, is identity.
Within one policy, two ACTIVE rules with the same enforcement key are
forbidden. That key is direction, address family, protocol, port range, remote
selector, and action; description is non-enforcement metadata. A duplicate
attempt conflicts without mutation, and the uniqueness check must be durable
and transactional. Remote-group and remote-address-group selectors are
deferred until a canonical dynamic membership/reference model exists.

## PolicyIntent and compilation

`PolicyIntent` remains a compiled endpoint-specific execution intent containing
endpoint identity, rule semantics, and execution-generation context. It does
not own reusable policy lifecycle, rule identity, attachment desired state, or
project-level Security Group existence.

```text
NetworkPolicy + Rules + Attachments + Endpoint/Realm snapshot
  -> deterministic compiler -> endpoint PolicyIntent[]
  -> NetworkPlanIntent::Policy -> selected execution provider
```

Provider observations and compatibility rows cannot create, resurrect, or
override canonical policy, rule, or attachment state.

## Security posture and P13 resolution

The current execution behavior is explicit and must not be silently
reinterpreted:

- an Endpoint with no attached policy has no targeted policy rule;
- an attached policy with zero rules has no targeted policy rule;
- the current nftables provider uses a marked chain with host policy `accept`,
  so unmatched traffic remains accepted subject to other O3K network controls;
- current `PolicyIntent` rules may Allow or Deny and are emitted in snapshot
  order; established/related conntrack traffic is accepted before targeted
  rules;
- this endpoint execution behavior is not evidence of Neutron Security Group
  semantics and is not changed by this proposal.

Neutron Security Groups are stateful allow-list projections and commonly carry
provider-created default egress rules. The proposed P13 boundary keeps
detached and zero-rule policies inert, gives only explicitly represented rules
policy effect, and requires positive/negative traffic evidence before
compatibility acceptance. Any change from the current O3K unmatched-traffic
posture is a separate human security decision, not an implication here.

The canonical model retains Allow and Deny so future O3K profiles are not
distorted to Neutron's allow-only vocabulary. `unmatched_action=Allow` permits
traffic matching no explicit rule; `unmatched_action=Deny` rejects such new
traffic. These values are canonical desired state and are persisted in every
policy generation.

For one Endpoint, all simultaneously active attached policies must use the
same `unmatched_action`; an attachment or update that would disagree is
rejected before provider mutation. Explicit rules from policies sharing that
default are evaluated as one set: any matching Deny wins, otherwise any
matching Allow wins, otherwise the shared unmatched action applies. Attachment
order is never authority. Established/related traffic may be admitted before
new-flow evaluation only when the selected stateful provider supports it; it
must not bypass `unmatched_action=Deny` for a new flow.

An Endpoint with no attached policy retains the existing O3K baseline and does
not acquire a policy default. A policy with zero rules permits unmatched
traffic when its action is Allow and denies new unmatched traffic when its
action is Deny. `stateful=true` is the only proposed P13 mode; stateless mode
is deferred.

The future Neutron Security Group projection sets `stateful_mode=Stateful` and
`unmatched_action=Deny`, and projects each Neutron rule to an explicit
canonical Allow rule. It must not inject a compatibility-only terminal drop.

## Lifecycle, persistence, and concurrency

Policy lifecycle is `requested -> active -> deleting -> deleted`, with
`error` following existing conventions. Rule and attachment mutations publish
one complete policy generation. Deleting an attached policy is a deterministic
conflict; deletion does not silently detach live endpoints. Rule deletion
targets its UUID.

The eventual SQLite and PostgreSQL schema must enforce project ownership,
parent existence, no orphan rules, same-project attachments, UUID identity,
generation fencing, and deterministic ordering. Database constraints and
transactions, not process-local locks, decide concurrent create/update/
attach/detach/delete/replay outcomes. Restart reconstructs canonical policy
state from these rows and fails closed on invalid relations.

Required race cases are concurrent rule creation and duplicate semantic rules;
rule deletion versus policy deletion; attachment versus policy or Endpoint
deletion; stale-generation compilation; restart during publication; and
unknown provider application outcomes. Policy deletion never deletes an
Endpoint. Endpoint deletion removes only its own attachments.

## Existing state and migration

Existing endpoint-scoped policy rows are legacy or derived execution state for
this proposal. They are not automatically promoted into reusable policies. A
future migration must prove project-level intent and identity; otherwise the
legacy path remains separate. No migration is authorized here.

## Native and compatibility consequences

Future native operations are policy CRUD, rule CRUD, and attachment add/remove.
A later OpenStack adapter may map Security Group to NetworkPolicy, Rule to
NetworkPolicyRule, and Port membership to PolicyAttachment, using normal
AuthContext authorization. This proposal adds no API routes, migrations,
types, Neutron behavior, Port attachment fields, routers, floating IPs,
volumes, or P13.4 work.
