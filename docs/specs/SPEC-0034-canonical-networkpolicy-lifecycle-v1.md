# SPEC-0034 — Canonical NetworkPolicy / PolicyRule / PolicyAttachment lifecycle v1

Status: Proposed
Date: 2026-08-26
Decision: [ADR-0177](../adr/ADR-0177-canonical-networkpolicy-and-reusable-policy-set.md)
Applies-to: canonical policy domain, persistence, compiler, execution, future native and compatibility adapters

This proposal is a design target, not an implementation or support claim.
Human acceptance of ADR-0177 and this specification is required before runtime,
schema, migration, or compatibility work begins.

## 1. Purpose and authority

O3K needs a reusable project-owned policy that remains valid before Endpoint
attachment. O3K owns policy/rule identity, project scope, desired state,
generation, authorization, lifecycle, and reconciliation. OpenStack Security
Groups are a future projection. `PolicyIntent` remains endpoint-targeted
execution input and is not promoted or renamed here.

## 2. Resources

`NetworkPolicy` has `id`, `project_id`, bounded `name` and `description`,
`state` (`requested`, `active`, `deleting`, `deleted`, `error`), positive
`generation`, canonical timestamps, `stateful_mode`, and
`unmatched_action` (`allow` or `deny`). It has independent CRUD, import, and
detached existence. `unmatched_action=allow` permits new traffic with no
matching explicit rule; `unmatched_action=deny` rejects it.

`NetworkPolicyRule` has independent UUID `id`, `policy_id`, `project_id`,
direction, address family, protocol, optional port range, remote selector,
Allow/Deny action, lifecycle state, and generation. Rule order is not identity.
Within one policy, ACTIVE rules must have unique enforcement keys consisting of
direction, address family, protocol, port range, remote selector, and action;
description does not affect equality. A duplicate create conflicts with no
mutation, enforced transactionally.

`PolicyAttachment` has independent UUID `id`, `policy_id`, `endpoint_id`,
`project_id`, lifecycle state, and generation. Active attachments require
active same-project policy and Endpoint. The domain permits many-to-many policy
and Endpoint relations; provider cardinality is profile admission only.

## 3. Invariants

- policy, rule, and attachment ownership is durable and project-scoped;
- rule and policy projects match;
- attachment, policy, and Endpoint projects match;
- parent and referenced resources are active before attachment;
- UUID knowledge never authorizes access;
- compatibility rows, provider handles, and compiled intents cannot alter
  canonical identity, ownership, or existence.

## 4. Lifecycle and deletion

Policy uses `requested -> active -> deleting -> deleted`, with `error` for
recoverable failures. Rule and attachment lifecycles use the same durable
state convention where recovery requires it. Policy deletion conflicts while
rules or active attachments remain; it does not silently detach live Endpoints.
Rule deletion removes exactly its UUID. Endpoint deletion removes only its own
attachments and never the policy or its rules.

## 5. Generation and compilation

Each policy/rule/attachment mutation is authorized against the current
generation and publishes one complete snapshot. The compiler consumes:

```text
canonical policy + rules + attachments + Endpoint/Realm snapshot
  -> deterministic endpoint PolicyIntent[]
  -> NetworkPlanIntent::Policy
  -> selected stateful policy provider
```

Stale compilers cannot overwrite newer generations. Provider state is never
read backward into canonical state, and an address is interpreted only with an
established Realm context.

## 6. Security and bounded P13 projection

Current execution semantics are: no attached policy and an attached policy
with zero rules both produce no targeted policy rule; the marked provider chain
has host policy `accept`; explicit current rules may Allow or Deny; and
established/related conntrack traffic is accepted before targeted rules. This
is legacy endpoint execution behavior, not a claim of Neutron equivalence.

The proposed P13 boundary keeps detached policies inert and makes zero-rule
semantics explicit: an attached Allow-default policy permits unmatched new
traffic, while an attached Deny-default policy rejects it. An Endpoint with no
attached policy retains the existing O3K baseline. A canonical policy retains
Allow and Deny; any matching Deny wins, otherwise any matching Allow wins,
otherwise `unmatched_action` applies. Established/related traffic may be
accepted before new-flow evaluation only under selected stateful-provider
semantics and must not bypass a Deny default for a new flow. Any change to the
global O3K baseline requires a separate human security decision.

Neutron Security Group projection uses `stateful=true` and
`unmatched_action=deny`, mapping Neutron rules to explicit canonical Allow
rules. It does not inject a compatibility-only terminal drop. Neutron default
rules are not materialized implicitly.

The bounded first projection may support stateful=true, IPv4, ingress/egress,
any/TCP/UDP/ICMP, valid ports, remote CIDR, and explicit allow rules only
after canonical semantics are proven equivalent.

IPv6, stateless mode, arbitrary protocol numbers, remote groups, remote address
groups, and policy-reference selectors are deferred.

## 7. Persistence, restart, and migration

The eventual SQLite/PostgreSQL schema has independent policy/rule/attachment
relations, foreign keys, indexes, generation/state, and selected active
uniqueness constraints. Reconstruction loads rows in stable UUID order and
fails closed on orphan, foreign-project, duplicate-active, or generation-
inconsistent state. It never creates policy from PolicyIntent, nftables, or
Neutron projection rows.

Existing endpoint-scoped rows are legacy/derived execution state. No automatic
one-policy-per-Endpoint migration is allowed. A future migration must be
transactional, restartable, and preserve identity only where identity is
proven, independently on SQLite and PostgreSQL.

## 8. Future API and projection

Future native operations are policy CRUD, rule CRUD, and attachment add/remove.
A later accepted compatibility profile may map SecurityGroup to NetworkPolicy,
SecurityGroupRule to NetworkPolicyRule, and Port membership to
PolicyAttachment. This specification enables none of those routes.

Policy deletion conflicts while rules or active attachments remain. Rule
deletion removes the selected durable rule UUID. Endpoint deletion removes
only its own attachments; it never deletes the policy or its rules. Policy and
attachment IDs survive restart and import through canonical persistence.

## 9. Required gates

Before runtime acceptance, tests must cover ownership, identity, duplicate
rules, concurrent rule creation, attachment/delete dependencies, generation
races, restart during publication, compiler determinism, provider
failure/rollback, unknown outcomes, and positive/negative traffic. The later
provider gate must independently prove the pinned OpenTofu/provider behavior
and every advertised field.
