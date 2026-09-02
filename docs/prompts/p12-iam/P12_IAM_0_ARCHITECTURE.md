# P12-IAM.0 — Architecture and Federation Security Contract

## Goal

Freeze the production federation architecture before semantic runtime changes.

## Preconditions

- P13 #744 and P13.7 #752 are closed.
- Start from the then-current protected `origin/main`.
- Re-audit the current IAM code; do not assume the September 2026 planning snapshot is unchanged.

## Required audit

Inspect and map:

- `o3k-identity` token issuance/validation;
- native `POST /o3k/v1/identity/tokens` and `GET /o3k/v1/identity/me`;
- `AuthContext`, `Principal`, `OwnershipScope`, `ScopeKind`;
- durable identity repositories/migrations;
- Keystone compatibility mapping;
- role assignments and authorizer behavior;
- service/delegation identity;
- audit propagation;
- API/OpenAPI/schema contracts current at execution time.

Produce a current-state boundary map and explicit gap matrix.

## Architecture to select

Define a generic federation port in O3K IAM capable of validating a trusted external OIDC access token and resolving it to a canonical O3K principal. Keep protocol/wire validation outside service business logic.

The accepted target must preserve:

```text
External OIDC access token
 -> trusted issuer validation
 -> external identity (issuer, sub)
 -> durable O3K PrincipalId binding
 -> authorized O3K scope
 -> native O3K token
 -> canonical AuthContext
```

Define separate concepts for:

- trusted issuer configuration;
- validated external identity;
- durable federated binding;
- scope assignment/discovery;
- native token exchange;
- system/operator assignment.

## Security decisions required

Freeze at minimum:

- access token, not ID token, as federation evidence for API exchange;
- accepted signing algorithms allowlist;
- issuer exact-match rules;
- audience validation;
- `exp`, `nbf`, bounded clock skew;
- JWKS discovery/cache/rotation semantics;
- `kid` miss/refresh behavior;
- maximum token/JWKS sizes and network timeouts;
- external subject identity key `(issuer, sub)`;
- behavior for unknown binding, disabled principal, disabled scope, removed assignment;
- non-enumerating failures;
- audit identity/correlation;
- restart behavior;
- failure behavior when issuer/JWKS is unavailable.

Do not design browser cookies, callback routes or Araf state into O3K.

## Deliverables

Create an accepted ADR and normative specification, with repository-conventional numbering selected at execution time. Update `NORMATIVE_SOURCES.md`, ADR index and architecture boundaries as required.

The spec must define API semantics but should avoid prematurely freezing a route shape before the domain/security design is accepted.

## Tests/gates

This slice is primarily architecture. Run governance/architecture checks and all required repository gates for documentation/contract changes. If small compile-time scaffolding is introduced, it must contain no unreviewed semantic behavior.

## Stop conditions

Stop and request architecture review if:

- federation would require email/name as canonical identity;
- system access would depend on username/admin strings;
- O3K would need to become a browser session server;
- existing AuthContext cannot represent the required result without a broader IAM change;
- the design weakens Keystone/native-token semantics.

## Completion report

Report exact baseline SHA, files inspected, gap matrix, accepted authority model, non-goals and findings. Verdict:

`P12-IAM.0 architecture gate: PASS|BLOCKED`
