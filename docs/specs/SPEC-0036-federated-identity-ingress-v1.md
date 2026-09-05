# SPEC-0036 — Federated identity ingress v1

Status: Accepted
Accepted-by: project-requester on 2026-09-06; architecture/security contract recorded in GitHub #794
Decision: [ADR-0179](../adr/ADR-0179-o3k-federated-identity-ingress.md)
Applies-to: O3K IAM federation adapter, native identity ingress, canonical AuthContext

This specification defines the domain and security contract for P12-IAM. It is
deliberately independent of a particular IdP, web framework, browser client or
OpenStack service process. Concrete route and schema artifacts are selected in
P12-IAM.6 after the domain contract is implemented.

## 1. Typed federation concepts

`TrustedIssuer` is configured O3K trust state: stable trust ID, exact issuer,
required audience, allowed signing algorithms, discovery/JWKS policy, bounded
timeouts/sizes, and clock-skew policy. Configuration is validated at startup
or safe reload and invalid profiles fail closed.

`ValidatedExternalIdentity` contains only the canonical trusted issuer ID or
issuer URL, OIDC `sub`, and bounded safe authentication metadata needed for
audit. It does not contain the raw access token or arbitrary claims.

`FederatedBinding` durably maps one `(trusted_issuer_id, sub)` pair to one
existing O3K `PrincipalId`. The pair is unique and immutable for the binding.
Bindings carry lifecycle state and safe created/updated audit metadata.

`DiscoverableScope` is a public-safe projection of a currently effective O3K
assignment: stable scope ID, supported scope kind, and safe display metadata.
It never exposes private policy internals or unrelated tenant existence.

`FederatedExchange` requests a native O3K token for one discoverable scope.
The requested scope is checked against current O3K assignment state immediately
before issuance.

## 2. Validation contract

The federation adapter MUST:

1. select a configured trust profile without trusting issuer/JWKS URLs from the
   credential;
2. validate an access token's compact representation, signature, algorithm,
   exact issuer, required audience, subject, `exp`, and `nbf` when present;
   The initial JWT access-token profile requires the RFC 9068 `typ` header
   value `at+jwt`; an ID-token-shaped JWT is not accepted as an API credential.
3. use a maintained standards-based JOSE/OIDC implementation;
4. bound input, metadata, JWKS, network timeout and refresh work;
5. refresh at most once for an unknown key ID when the configured policy allows
   it, then fail closed;
6. return one non-enumerating authentication failure for malformed, untrusted,
   expired, unavailable or otherwise invalid credentials.

The adapter MUST NOT accept `alg=none`, an ID token as an API credential,
dynamic issuer trust, token-provided key locations, or mutable claims as
canonical identity. Raw credentials MUST NOT enter durable state, ordinary
logs, audit payloads or public error details.

The initial production profile requires HTTPS metadata/JWKS. Local HTTP is
permitted only by an explicitly named test profile and is not production
evidence.

## 3. Binding and lifecycle contract

Binding resolution is exact on `(trusted_issuer_id, sub)`. The same pair always
resolves to the same PrincipalId; the same subject under another issuer is a
different identity. Duplicate insertion is rejected or idempotently returns the
same binding, never a second principal mapping. A binding cannot reference a
missing principal.

Explicit provisioning is the initial profile. An unknown pair, disabled
binding, disabled principal or malformed relationship fails closed without
revealing which record was absent. Changing an IdP email, name or group cannot
change the binding. Disable/removal affects new exchanges according to the
durable transaction boundary and never grants access through cached display
metadata.

SQLite and PostgreSQL adapters MUST preserve uniqueness, referential integrity,
lifecycle and restart behavior equivalently where each backend is enabled.

## 4. Scope and exchange contract

Scope discovery is authenticated by the validated external identity's canonical
O3K binding and derives only from current O3K assignments. It omits scopes for
which the principal has no effective assignment or the scope is disabled. A
requested ID not in the discoverable set receives a non-enumerating denial.
Large assignment sets use bounded server-side pagination when the public API
slice selects enumeration.

Rescoping is a new authorization decision, not a copy of a previous
`AuthContext`. It revalidates the external credential/binding, principal,
scope, assignment and current policy. A normal tenant never receives system
scope from a project assignment. Native token-to-token reauthentication remains
the existing credential method and is not silently redefined.

On successful exchange, the IAM service issues the existing native O3K token
model. Downstream services receive the existing canonical `AuthContext` with
PrincipalId, effective OwnershipScope, role/policy inputs, expiry and audit /
request correlation. The native credential MUST NOT outlive the selected
external-credential lifetime policy, and no IdP refresh token is stored by O3K.

## 5. System/operator profile

System scope is a separate O3K authorization profile. It is granted only by a
durable explicit operator assignment to a canonical human principal (or by a
separately accepted service/delegation contract). Username, email domain,
project-admin role, client application, browser session and IdP group claims
cannot grant it.

The operator action/resource matrix is published before implementation of the
operator slice. Every selected operator route performs server-side action,
resource and scope authorization; route discovery never grants access. A
service principal remains a service principal and cannot become a system human
principal through federation.

## 6. Failure and audit contract

Federation failures use the native RFC 9457 Problem Details family and stable
machine codes selected by the public-contract slice. Authentication and scope
failures do not distinguish unknown issuer, subject, binding, principal,
assignment, project or operator state to an untrusted caller.

Audit records may correlate a decision with trusted issuer ID, a safe subject
fingerprint/reference, canonical PrincipalId, requested scope, action,
correlation ID and allow/deny result. They MUST exclude access tokens, native
tokens, refresh tokens, client secrets, private keys and unredacted provider
payloads.

## 7. Compatibility and evidence gates

Federation is an additional IAM ingress. It does not change existing Keystone
token request/response semantics, native password authentication, native token
reauthentication, project ownership, service identity or P13 isolation. Any
wire change must be versioned and represented in the public contract.

P12-IAM.1–.6 provide contract and implementation evidence. P12-IAM.7 is the
real-provider gate and must prove tenant/operator isolation, failure handling,
restart, persistence and key rotation. P12-IAM.8 must prove the Araf BFF
separation and production-auth journey. Architecture acceptance alone is not
runtime or interoperability evidence.
