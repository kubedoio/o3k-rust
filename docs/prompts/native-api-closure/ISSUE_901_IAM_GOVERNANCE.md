# Implementation Prompt — Issue #901

## Mission

Implement **#901 — Add canonical IAM governance and project membership administration** on this branch without redesigning the already-proven P12-IAM authentication/token-exchange path.

## Baseline

This branch is refreshed directly onto current authoritative `main` and carries
only this planning prompt. Do not inherit planning state — including old
governance/quota/audit/metering prototypes — from any earlier draft branch.
Start from current `main` and the issue text.

## Authority audit first

Refresh from current authoritative `main`, read #901 and the accepted P12-IAM
ADR/spec/evidence. Map durable projects, principals, roles, assignments, service
principals, operator assignments, AuthContext/action policy, federated scope
discovery and Keystone-compatible administration overlap.

Consult at minimum:

- `docs/adr/ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md`;
- `docs/prompts/p12-iam/` and `docs/plan/p12-iam-production-federation-plan.md`
  (especially `P12_IAM_5_SYSTEM_OPERATOR.md` and `P12_IAM_8_ARAF_CONVERGENCE.md`);
- `docs/specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md`;
- merged durable audit: `docs/specs/SPEC-0042-durable-audit-v1.md`;
- merged operator authorization pattern: `docs/specs/SPEC-0043-native-quota-v1.md`
  and `bins/o3kd/src/native_adapters/quota.rs`.

External IdP identity is not cloud authorization truth. Do not use Keycloak/IdP
group/role strings as O3K membership/system authority and do not create
Araf-local IAM state.

## Required implementation

Define versioned native governance contracts for the subset current O3K can
authoritatively support:

- bounded system-authorized project/scope list/show;
- bounded canonical principal list/show with safe identity metadata;
- bounded project membership/role-assignment list;
- assignment create/delete/equivalent mutation with deterministic
  idempotency/race behavior;
- canonical role/effective capability/action projection so clients do not infer
  permissions from display role names;
- service/workload principal administration only where already canonical and
  secret-safe;
- explicit operator assignment administration only if it can preserve P12-IAM
  durable system authority safely.

Do **not** invent Account/Organization/customer hierarchy in this issue. Do not
expose reusable service/user credentials in list/show responses.

## Operator authorization pattern (follow merged #900)

The merged native quota surface (SPEC-0043) is the reference for
system/operator-scoped native administration: system-only operator routes live
under an explicit operator path, every operator action maps to a named canonical
action (`quota:ReadQuota`, `quota:ManageQuota`), effective tenant scope is
always taken from `AuthContext`, and mutations use a durable compare-and-set with
an explicit precondition. Reuse these patterns for governance mutation instead
of inventing a separate authorization style.

## Security requirements

Prove:

- ordinary tenant cannot enumerate global projects/principals/assignments;
- project admin cannot self-promote to system/operator by default;
- grantor cannot grant authority it is not allowed to grant;
- cross-project administration follows explicit canonical actions/policy;
- IdP claims alone cannot create durable O3K project/operator authority;
- foreign IDs do not become an unintended enumeration channel;
- service credentials/password hashes/private keys/raw tokens are structurally
  absent;
- duplicate concurrent/replayed assignment requests converge or conflict
  deterministically;
- governance mutations are durable-auditable with actor/target/scope/correlation
  through the merged durable audit contract (SPEC-0042), not a new audit path.

## Scale

All project/principal/assignment collections must be indexed and
repository-bounded for SQLite/PostgreSQL. Use strict limits/query-bound opaque
cursors. No global load-and-filter implementation.

## Convergence

Native and selected Keystone-compatible administration must share the same
durable IAM authority. Add convergence tests; do not fork records/models just to
simplify native DTOs.

## Contracts / validation

Add/adjust ADR/SPEC/public schemas and action registry entries with least
privilege. Run full Rust gates, identity/store migration/conformance, relevant
PostgreSQL ignored tests, real federated tenant/system process tests,
privilege-escalation/two-tenant negatives and compatibility convergence.

## Forbidden shortcuts

Do not rewrite P12-IAM login/token exchange. Do not use frontend role-name
inference. Do not store IdP passwords. Do not expose arbitrary policy-expression
editing. Do not make browser custody of service secrets necessary.

## Stop condition

Only report `BLOCKED` for a genuine external dependency outside `o3kio/o3k`.
Cross-crate IAM/store/API/policy migrations needed to satisfy #901 are part of
the implementation.

Finish only with `#901 COMPLETE` when issue exit criteria and production process
evidence are proven.
