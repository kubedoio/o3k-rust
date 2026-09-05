# ADR-0179 — O3K federated identity ingress and OIDC trust boundary

Status: Accepted
Date: 2026-09-06
Human-approval: project-requester, P12-IAM execution authorization recorded in GitHub #794
Reviewed-baseline: ed616688d73d6f3a89a497cb2baa5e7c4e2c6d1e
Supersedes: none
Superseded-by: none
Affected-services: identity, kernel, native-api, store, governance

Related decisions and specifications:

- [ADR-0165 — O3K Cloud OS and Cloud Kernel](ADR-0165-o3k-cloud-operating-system-and-cloud-kernel.md)
- [ADR-0166 — O3K IAM and Keystone compatibility boundary](ADR-0166-o3k-iam-and-keystone-compatibility-boundary.md)
- [SPEC-0020 — Keystone trust, catalog and AuthContext](../specs/SPEC-0020-keystone-trust-catalog-and-auth-context.md)
- [SPEC-0036 — Federated identity ingress v1](../specs/SPEC-0036-federated-identity-ingress-v1.md)

## Context

P12-IAM adds a production identity ingress for confidential clients such as an
Araf BFF. The ingress must accept a standards-based OIDC access token without
turning an external identity provider, browser session, or client claim into
O3K cloud authority. O3K already has a durable Keystone-shaped identity
snapshot, native password/token issuance, HMAC-signed native credentials,
`AuthContext`, project ownership checks, service principals, delegated service
identity, and audit/request identifiers.

The implementation baseline was audited at
`ed616688d73d6f3a89a497cb2baa5e7c4e2c6d1e`.

## Current-state boundary map and gap matrix

| Existing authority | Current implementation | P12-IAM gap | Slice |
|---|---|---|---|
| Canonical principal and scope | `o3k-kernel::{principal,scope,auth_context}`; `AuthContext` carries principal, effective scope, roles, expiry and audit/request IDs | No external identity value object or system-scope authorization profile | .0, .2, .3, .5 |
| Native/Keystone authentication | `o3k-identity::TokenService` issues and verifies HMAC native tokens from password or existing token; native API and Keystone adapters call it | No federated credential method; existing `token` method must retain re-authentication semantics | .4 |
| Durable IAM state | `o3k-store` `IdentityRepository` and SQLite/PostgreSQL Keystone tables for domains, projects, users, roles and project assignments | No trusted issuer or `(issuer, sub)` binding tables/repository port | .1, .2 |
| Authorization | `o3k-kernel::Authorizer` evaluates typed action/resource/ownership requests; current standard policy is project-oriented | No authoritative federated scope discovery/rescoping or bounded system/operator action inventory | .3, .5 |
| Compatibility mapping | Keystone records and catalog are loaded by `o3k-identity`; `AuthContext` is reconstructed from verified native claims and durable state | Federation must terminate at IAM and project into the same context without changing Keystone wire authority | .4, .6 |
| Public native identity API | `POST /o3k/v1/identity/tokens` and `GET /o3k/v1/identity/me`; native request DTO and Problem Details exist | No public federated exchange/scope contract or machine-readable identity contract covering it | .3, .4, .6 |
| Service/delegation identity | `ServicePrincipal` and `AuthContext::service_principal`; P12 controller protocol carries delegated actor/scope/action | External human federation must never become a service principal or broaden delegation | .5 |
| Audit/correlation | Native context has generated `audit_id`/`request_id`; protocol adapters propagate request IDs | Federation needs safe issuer/subject correlation without credentials and explicit decision events | .1–.5 |
| Browser identity | No browser-session authority in O3K | Must remain outside O3K: redirects, callbacks, cookies, CSRF and browser storage belong to the confidential BFF | all |

This ADR records the gap, not an implementation claim. Runtime and support
claims remain gated by the sequential P12-IAM slices and their evidence.

## Decision

O3K IAM gains a generic federation port with this authority flow:

```text
external OIDC access token
  -> configured issuer/discovery/JWKS trust
  -> validated external identity (canonical issuer, sub)
  -> durable O3K binding to PrincipalId
  -> canonical O3K assignment and requested scope
  -> existing native scoped token
  -> existing AuthContext and Authorizer
```

The OIDC validator is an IAM adapter. O3K services never parse OIDC tokens,
external claims, cookies, or provider-specific group data. A validated external
identity is authentication evidence only; PrincipalId, assignments, scopes,
actions, ownership and authorization remain O3K state.

### Trust and validation

- Trust is an explicit configured issuer profile with a stable O3K trust ID.
- The credential for API exchange is an OIDC access token, never an ID token.
- Issuer comparison is exact after the accepted URL canonicalization; the
  token issuer must equal the configured issuer.
- O3K validates the required audience, `exp`, `nbf` when present, signature,
  key selection and an explicit algorithm allowlist. `none` and algorithms not
  selected by the profile are rejected.
- Metadata and JWKS are fetched only from trusted discovery/configuration, over
  HTTPS in production profiles, with bounded response size and timeout.
- A bounded refresh is allowed once for an unknown `kid`; repeated misses,
  unavailable metadata/JWKS, malformed data and trust mismatches fail closed.
- Token, header, claim, metadata and JWKS sizes are bounded. Raw credentials
  are not persisted, included in domain errors, or logged.
- Clock skew is bounded by the selected profile and is applied only to time
  validation; it cannot make an expired credential valid indefinitely.

### External identity and binding

The immutable external identity key is `(trusted_issuer_id, sub)`, where `sub`
is the OIDC subject and `trusted_issuer_id` identifies the exact configured
issuer. Email, username, display name, groups and mutable profile claims are
never identity keys or authorization inputs. A durable unique binding resolves
the pair to one existing canonical O3K PrincipalId. The first profile uses
explicit provisioning; unknown subjects do not create users or operators.

Bindings and principals are independently disableable. Disabled, missing or
invalid state produces a non-enumerating failure. Binding persistence must be
equivalent across SQLite and PostgreSQL where those profiles are supported.

### Scope and authorization

Scope discovery and rescoping re-read current canonical O3K assignments. A
client-supplied scope is a request, never authority. Project, domain and system
scope kinds are distinct; a kind is advertised only when its authorization
semantics are implemented and evidenced. System scope requires an explicit,
durable operator assignment and a bounded action/resource profile. Tenant roles,
project-admin status, service principals and BFF session state do not imply
system authority.

The existing `AuthContext` remains the downstream type. A federated exchange
creates the same canonical principal/scope/role context as other native token
issuance, with safe authentication metadata and correlation identity. It does
not create a second service authorization model.

### Native token and compatibility boundary

Federated exchange is an explicit credential method or equivalent versioned
contract; it must not overload the existing native `token` re-authentication
method. The resulting native token follows the existing signing, validation,
expiry, restart and authorization semantics. Its lifetime is bounded by both
the native policy and the remaining external credential lifetime according to
SPEC-0036. Keystone-compatible routes and native password/token behavior remain
unchanged.

### Browser and Araf boundary

An Araf or other confidential BFF owns OIDC Authorization Code + PKCE/client
handling, redirects, callbacks, cookies, CSRF, browser session state and
server-side storage of external/native credentials. O3K receives a credential
for federation and returns native API results. Browser cookies and frontend
state are never accepted as O3K authorization authority, and no Araf-specific
endpoint or business rule is added to O3K.

## Consequences and non-goals

This decision creates a provider-neutral IAM port and durable identity
contracts, but does not claim OIDC interoperability, production readiness or
operator-console readiness until the later evidence slices pass.

It explicitly does not add SAML, SCIM, generic OAuth authorization-server
behavior, social-login APIs, group synchronization, email auto-enrollment,
browser session storage, refresh-token storage, or a new historical OpenStack
service boundary.

## Required evidence

Each semantic slice must include domain, security, persistence and compatibility
tests appropriate to its boundary. P12-IAM.7 must exercise a real standards-
based IdP and persisted O3K state; P12-IAM.8 must prove the real Araf BFF
boundary. The final claim is permitted only with the exact closure verdicts in
the P12-IAM plan.
