# P12-IAM — Production IAM, OIDC Federation & Console Identity

## Status

**PLANNED — DO NOT IMPLEMENT UNTIL P13 IS CLOSED.**

Execution prerequisite:

- P13 umbrella #744 closed;
- P13.7 #752 closed with accepted final evidence;
- implementation starts from the then-current protected `origin/main`.

P12-IAM is a focused convergence round built on the IAM architecture introduced in P12. It is intentionally named `P12-IAM` because it completes the production identity surface of that architecture. It is not chronologically inserted before P13.

## Product goal

Provide a production-grade, service-neutral O3K identity ingress for human cloud users and operators so native clients such as Araf can authenticate through standard OIDC federation while O3K remains the authority for principals, scopes, authorization, native tokens and `AuthContext`.

The target boundary is:

```text
Browser
   |
   v
Araf BFF / another confidential client
   |
   | OIDC Authorization Code + PKCE handled by the client/BFF
   | external OIDC access token
   v
O3K federated IAM
   |
   +-- validate trusted issuer/JWKS/audience/time/algorithm
   +-- resolve durable (issuer, subject) -> O3K PrincipalId
   +-- discover/validate authorized O3K scopes
   +-- issue native scoped O3K token
   v
canonical AuthContext
   |
   v
O3K services
```

O3K does **not** own browser cookies, Araf callback URLs, React authentication state or a browser session store. Those remain client/BFF responsibilities.

## Authority rules

Non-negotiable:

- external IdP identity is evidence used to authenticate an O3K principal; it is not O3K resource authority;
- `(issuer, subject)` is the external identity key; display name/email are not security identities;
- O3K `PrincipalId`, assignments, scope and authorization remain canonical;
- project/domain/system scope comes from O3K, not client claims;
- native O3K tokens and `AuthContext` remain the contract consumed by O3K services;
- existing Keystone compatibility remains a projection of the same IAM authority;
- no Araf-specific IAM model or endpoint may be introduced;
- unsupported federation state fails closed;
- federation must not weaken cross-project isolation, delegation, audit or token validation.

## Phase sequence

| Slice | Scope | Exit condition |
|---|---|---|
| P12-IAM.0 | Architecture and security contract | ADR/spec accepted before semantic code |
| P12-IAM.1 | Trusted OIDC issuer + access-token validation | External access token validates fail-closed against configured trust |
| P12-IAM.2 | Durable federated subject binding | `(issuer, sub)` resolves to stable O3K principal across restart |
| P12-IAM.3 | Scope discovery and authorized rescoping | Caller can discover/select only assigned O3K scopes |
| P12-IAM.4 | Federated exchange to native scoped token | External token exchanges to canonical native token/AuthContext |
| P12-IAM.5 | System/operator authorization | Real system-scoped operator identity, no username/admin magic |
| P12-IAM.6 | Public machine-readable IAM contract | OpenAPI/JSON Schema and compatibility rules are published |
| P12-IAM.7 | Real IdP integration/security evidence | Real standards-based IdP proves tenant/operator isolation |
| P12-IAM.8 | Araf convergence and closure | Araf production-auth path works without fixture identity |

## Reference profile

A deterministic reference environment may use Keycloak because it is easy to automate, but Keycloak is only a test provider. The canonical implementation must use standards-based OIDC behavior and remain compatible with other conforming providers.

Reference principals:

- Alice -> Project A only;
- Bob -> Project B only;
- Cloud Operator -> explicit system/operator authorization;
- optional service principal remains separate from human federation.

## Explicit non-goals

P12-IAM does not require:

- organization/account hierarchy;
- SCIM;
- SAML;
- social-login provider-specific APIs;
- JIT user provisioning unless separately accepted;
- IdP group-to-role automation unless separately accepted;
- browser cookies in O3K;
- Araf session storage in O3K;
- generic OAuth authorization-server functionality;
- replacing the existing native O3K token model;
- replacing Keystone compatibility;
- passwordless/FIDO/WebAuthn implementation inside O3K;
- AWS IAM syntax;
- application credentials/trusts unless separately selected.

## Required final evidence

P12-IAM is complete only when a real IdP/reference environment proves:

```text
Alice -> OIDC -> Project A AuthContext              PASS
Alice -> Project B                                  DENIED
Bob -> Project A                                    DENIED
Operator -> System AuthContext                      PASS
Alice -> operator/system route                      DENIED
expired token                                       DENIED
wrong issuer                                        DENIED
wrong audience                                      DENIED
invalid signature                                   DENIED
unknown external binding                            DENIED
unauthorized requested scope                        DENIED
O3K restart -> binding/token contract remains sane  PASS
Araf BFF -> exchange -> /identity/me                PASS
```

The final closure report must state exactly one:

- `P12-IAM aggregate verdict: PASS`
- `P12-IAM aggregate verdict: BLOCKED`

and separately:

- `Araf production identity unblocked: YES|NO`
