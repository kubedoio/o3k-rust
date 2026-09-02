# P12-IAM.7 — Real OIDC IdP Integration and Security Evidence

## Goal

Prove the P12-IAM federation profile against a real standards-based OIDC provider and real persisted O3K identity state. Unit tests and synthetic JWTs are insufficient for this gate.

## Preconditions

- P12-IAM.0–.6 merged.
- Start from current protected `main`.

## Reference environment

Build a deterministic testbed using a standard OIDC provider such as Keycloak. Keycloak is a reference implementation only; canonical O3K code must remain provider-neutral.

Reference topology:

```text
OIDC IdP
  |-- Tenant confidential client / Araf-like BFF harness
  `-- Operator confidential client / Araf-like BFF harness
                |
                v
              O3K
                |
            PostgreSQL
```

Use HTTPS/TLS in the production-like path. Test-only localhost exceptions must be explicit and cannot count as the sole production evidence.

Seed at minimum:

- Alice external subject -> O3K user principal -> Project A assignment;
- Bob external subject -> O3K user principal -> Project B assignment;
- Cloud Operator external subject -> O3K principal -> explicit system/operator authorization;
- no cross-assignment between A/B;
- no implicit operator status for tenant users.

## Required real flows

Tenant:

```text
Alice authenticates at IdP
 -> client receives OIDC access token
 -> O3K federation validates token
 -> scope discovery returns Project A only
 -> client exchanges for Project A native token
 -> /identity/me returns canonical Project A AuthContext
 -> Project A native resource read succeeds
 -> Project B read/mutation/Operation access denied
```

Operator:

```text
Cloud Operator authenticates
 -> system scope is discoverable/authorized
 -> native system token/AuthContext
 -> bounded operator API succeeds
```

Negative:

```text
Alice -> system/operator API -> DENIED
Bob -> Project A -> DENIED
```

## Failure/security matrix

Exercise with a real provider or faithful controlled boundary:

- expired access token;
- wrong audience;
- wrong issuer;
- invalid signature;
- unknown subject binding;
- disabled binding;
- disabled principal;
- disabled project;
- removed project assignment;
- removed operator assignment;
- IdP/JWKS temporary unavailability;
- key rotation;
- unknown `kid` recovery/failure;
- O3K restart;
- PostgreSQL restart where relevant;
- concurrent exchanges;
- malformed/oversized credential;
- tenant access to operator route.

## Evidence discipline

Record exact:

- O3K HEAD SHA;
- IdP product/version;
- OIDC discovery metadata relevant to the test (no secrets);
- issuer/audience configuration;
- database backend;
- client profile;
- exact PASS/FAIL matrix;
- logs demonstrating correlation/redaction;
- restart/key-rotation results.

Never upload access tokens, refresh tokens, client secrets, private keys or passwords.

## Real-provider portability

At minimum review the implementation against generic OIDC standards assumptions. If practical, run a second standards-compliant IdP smoke test; if not, explicitly state that Keycloak is the only executed provider and do not claim broader certified interoperability.

## Completion verdict

Exactly one:

`P12-IAM.7 real federation evidence: PASS`

or

`P12-IAM.7 real federation evidence: BLOCKED`
