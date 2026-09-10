# SPEC-0044 — Native IAM governance v1

Status: accepted target contract for #901 (implementation in progress).

## Authority boundary

Native IAM governance administers the **existing canonical durable O3K IAM
authority** and nothing else:

- `keystone_domains`, `keystone_projects`, `keystone_users`, `keystone_roles`;
- `keystone_role_assignments` (project membership / role assignment);
- `operator_assignments` (durable system/operator authority).

There is no second IAM database and no parallel native membership model. An
external IdP proves **external subject identity** only; it never becomes cloud
project/role/operator authority, and no IdP group or role claim creates durable
O3K authority. Araf, CLI, and SDK clients are consumers of this authority, never
authorities.

## Authorization

Every route requires an explicit system-scoped canonical action plus the durable
`operator` role (resolved from `operator_assignments`, exactly as P12-IAM system
tokens are):

- `governance:ReadGovernance` — collection and entity reads;
- `governance:ManageAssignment` — project membership/role-assignment mutation;
- `governance:ManageOperatorAssignment` — operator (system) assignment mutation.

All three are registered with expected resource type `governance:governance`,
accepted principal `User`, `require_ownership = false`, and required role
`operator`. `StaticAuthorizer` additionally rejects any of the three when the
effective scope is not `System` (`ScopeMismatch`), so a project-scoped caller
that happens to hold an `operator`-named role inside a project is still denied.
Ordinary tenants cannot enumerate or administer global IAM, and a caller cannot
grant authority it does not itself hold: grant authority is the system-scoped
action, never a role-name string comparison or a UI filter.

Routes are served by `o3kd` under `/o3k/v1/operator/governance/...`:

| Method | Path | Action |
| --- | --- | --- |
| GET | `/operator/governance/projects` | `governance:ReadGovernance` |
| GET | `/operator/governance/projects/{id}` | `governance:ReadGovernance` |
| GET | `/operator/governance/principals` | `governance:ReadGovernance` |
| GET | `/operator/governance/principals/{id}` | `governance:ReadGovernance` |
| GET | `/operator/governance/roles` | `governance:ReadGovernance` |
| GET | `/operator/governance/capabilities` | `governance:ReadGovernance` |
| GET | `/operator/governance/assignments` | `governance:ReadGovernance` |
| POST | `/operator/governance/assignments` | `governance:ManageAssignment` |
| DELETE | `/operator/governance/assignments/{id}` | `governance:ManageAssignment` |
| GET | `/operator/governance/operator-assignments` | `governance:ReadGovernance` |
| POST | `/operator/governance/operator-assignments` | `governance:ManageOperatorAssignment` |
| DELETE | `/operator/governance/operator-assignments/{id}` | `governance:ManageOperatorAssignment` |

Responses are versioned `v1`.

## Collections and representation

All collections are bounded at the repository boundary: the store applies a SQL
`LIMIT` of `page limit + 1` and orders by the stable canonical primary key.
There is no load-all-and-filter-in-application path. Page size is `1..=200`
(default 50). Continuations are opaque, HMAC-authenticated cursors that bind the
effective scope (`system`), the collection identity, and the full accepted filter
set, so a cursor issued under one scope or filter cannot be replayed under
another.

- **Projects** — canonical safe metadata only (`id`, `domain_id`, `name`,
  optional `description`, `enabled`, `created_at`). No organization, account,
  owner, or customer hierarchy is fabricated, and no project status mutation is
  exposed because the canonical model has no such durable semantics.
- **Principals** — canonical `keystone_users` (`id`, `domain_id`, `name`,
  `kind`, optional `email`, `enabled`, `created_at`). `kind` is `service` when
  the user holds the canonical `service` role in any project, else `user`; this
  is a projection of durable role membership, not a new identity store.
  `password_hash`, tokens, private keys, and service secrets are structurally
  absent from the type and can never be serialized.
- **Roles** — canonical role identity (`id`, `name`, optional `description`,
  `created_at`). Role names are display identity only.
- **Capabilities** — the canonical authorization action inventory projected from
  the kernel `Authorizer` (`action`, `resource_type`, `required_roles`,
  `require_ownership`). Clients must not infer permissions from role names; this
  projection reuses the canonical policy contract instead of introducing a
  second permissions model.
- **Assignments** — canonical `(principal_id, project_id, role_id, id,
  created_at)`, optionally filtered by any of the three references. Filters are
  pushed into SQL and bound into the cursor identity.
- **Operator assignments** — canonical (`id`, `principal_id`, `profile`,
  `enabled`, `created_at`, `updated_at`). Only the recognized `operator-console`
  profile is accepted; an unknown profile is a `BAD_REQUEST`.

## Mutation semantics

- **Assignment create** validates that the referenced principal and project
  exist and are enabled and that the role exists, in a single store check.
  Dangling references are rejected (`BAD_REQUEST`) rather than persisted; the
  durable `UNIQUE(user_id, project_id, role_id)` constraint prevents duplicate
  cloud authority. Creation is **idempotent**: replaying an identical assignment
  returns the existing canonical assignment and creates no new authority and no
  new audit event. Two concurrent identical creates converge to exactly one row
  through the durable unique constraint, never a process-local lock.
- **Assignment delete** removes the referenced assignment and returns `204`; an
  absent assignment is `RESOURCE_NOT_FOUND`.
- **Operator assignment create** validates the principal exists and is enabled
  and is likewise **idempotent** against the durable `UNIQUE(user_id, profile)`
  constraint. It is deliberately a distinct action so operator authority can
  never be granted through the ordinary assignment action.

## Audit

Every governance mutation commits its required durable `AuditEvent` **in the
same transaction** as the domain change (SPEC-0042). If the audit insert fails or
collides, the domain change is rolled back and the request fails; there is no
observable partial authority. The audit record carries the actor and effective
scope from the `AuthContext`, the action, the target assignment/operator
assignment identifier, the target project (or `system`) as the owner scope, and
the request/audit correlation identifiers. Audit records never contain
credentials. An idempotent replay that performs no write emits no audit event.

## Authorization convergence

Governance mutations change the durable IAM authority. The identity snapshot
that backs token issuance, federated scope discovery, and `AuthContext`
derivation is reloaded from durable storage after a successful governance
mutation, so a subsequent canonical re-authentication / token exchange / scope
discovery reflects the new membership and revocation without a process restart.
Reloads are serialized so an earlier commit's reload can never overwrite a later
commit's, and a reload failure after a committed mutation is surfaced to the
caller (fail-closed) rather than silently leaving stale in-process authority.

Distributed token revocation is explicitly out of scope; no #901-specific
recovery state is introduced.

### Existing-token semantics (TTL-bounded)

An **already-issued** token keeps its original lifetime. Its project `AuthContext`
roles are re-derived from the refreshed snapshot on the next verification, so a
role grant/revoke is reflected in role-based decisions. However, the canonical
authorizer decides owner-scoped actions by scope ownership, not by membership
roles: removing a principal's **last** membership in a project does not make
that principal's already-issued project token stop performing owner-scoped
actions (which carry no required role) until the token expires. New token
exchanges after revocation never receive the removed membership because
federated scope discovery excludes projects without assignments. Operators must
treat revocation of owner-scoped authority as bounded by the configured token
TTL; shortening TTL is the accepted control. Making removal of the final
membership immediately invalidate in-flight owner-scoped authorization would be
a P12-IAM authorization-model change and is deliberately not part of #901.

## Store parity

SQLite and PostgreSQL implement the same behavior through a shared
`GovernanceRepository` contract. Both apply the bounded keyset page, the same
unique-constraint conflict semantics, and the same same-transaction audit
commit. Migration `0044_governance_indexes` (SQLite) and
`0027_governance_indexes` (PostgreSQL) add the filtered-listing indexes; no new
IAM tables are created and existing memberships are preserved.

## Error semantics

Errors use RFC 9457 `application/problem+json` with the canonical native codes:
malformed request → `BAD_REQUEST`; missing authentication → `UNAUTHORIZED`;
insufficient scope/role → `FORBIDDEN`; absent (or non-visible) record →
`RESOURCE_NOT_FOUND`; invalid reference → `BAD_REQUEST`; unavailable/degraded
storage or audit → `NOT_AVAILABLE`; internal/corrupt state → `INTERNAL_ERROR`.
Responses never leak SQL, database URLs, tokens, credential material, provider
details, or private paths.

## Non-goals

Organization/account/customer hierarchy; IdP user lifecycle provisioning; IdP
password or token custody; arbitrary policy-expression editing; browser custody
of service secrets; rewriting P12-IAM authentication/token exchange; project
self-service administration by project-scoped callers.

The response schemas are in
[native-governance-v1.schema.json](../../contracts/native-governance-v1.schema.json).
