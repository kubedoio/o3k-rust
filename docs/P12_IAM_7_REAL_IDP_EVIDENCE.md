# P12-IAM.7 real federation evidence

Status: PASS for the executed reference-provider profile.

The executable gate is `tests/p12-iam-7-real-idp.sh`. It starts the pinned
Keycloak `25.0.6` reference provider, imports four deterministic users, obtains
real OAuth access tokens through the password grant used only by this testbed,
and runs `bins/o3kd/tests/p12_iam_7_real_oidc.rs`. The test seeds durable O3K
subject bindings and operator authorization, then exercises native exchange and
canonical `AuthContext` behavior.

Executed evidence:

- O3K HEAD: recorded by the PR and CI checkout;
- IdP: Keycloak 25.0.6, image digest pinned in the harness;
- issuer/discovery: discovered from the provider and checked by O3K;
- audience: `o3k`;
- algorithm: RS256;
- backends: persisted SQLite file and PostgreSQL 16.4 container;
- positive flows: Alice -> Project A and Cloud Operator -> system;
- denials: Alice -> Project B/system and Bob -> Project A;
- unknown and disabled durable subject bindings deny a valid IdP token;
- failures: wrong issuer, wrong audience, invalid signature, and unavailable
  provider fail closed without credential-bearing errors;
- restart: a fresh `TokenService` reloads the same durable bindings and
  successfully exchanges Alice again.

The testbed uses loopback HTTP deliberately and explicitly. It is not a
production TLS claim. Production composition rejects non-HTTPS remote issuer
and discovery configuration (`allow_insecure_local` is false); deployment
evidence must use an HTTPS provider with a normally trusted CA before claiming
production federation interoperability. Keycloak is the only executed
standards-based provider; no broader provider certification is claimed.

The validator accepts RFC 9068 `at+jwt` access tokens and the bounded generic
JWT/Bearer form emitted by this reference provider. An untyped ID-token-shaped
credential remains rejected. Raw access, refresh, and client credentials are
not written to the repository or evidence artifacts.

Exact verdict:

`P12-IAM.7 real federation evidence: PASS`
