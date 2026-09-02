# P12-IAM.4 — Federated Exchange to Native Scoped O3K Token

## Goal

Complete the production federation ingress: exchange a validated external OIDC access token for the existing native O3K scoped credential model and canonical `AuthContext`.

## Preconditions

- P12-IAM.1–.3 merged.
- Start from current protected `main`.

## Required contract

Extend native identity issuance with an explicit federated credential method or equivalent clean contract selected by the accepted spec.

Do not silently overload the existing native `token` method if it currently means reauthentication with an O3K-issued token.

Conceptual flow:

```text
external OIDC access token
 -> trusted issuer validation
 -> (issuer, sub)
 -> durable O3K principal binding
 -> requested O3K scope
 -> assignment/authorization validation
 -> native O3K scoped token
 -> GET /o3k/v1/identity/me
 -> canonical AuthContext
```

## Native-token invariants

The resulting credential must preserve existing native O3K semantics for:

- principal identity;
- effective ownership/security scope;
- role/policy inputs;
- issue/expiry;
- request/audit correlation;
- authorization checks consumed by services;
- restart validation according to the current token model.

Do not make O3K services understand OIDC tokens directly. Federation ends at IAM; downstream services continue consuming canonical `AuthContext`.

## Lifetime policy

Define a safe lifetime relationship between the external credential and issued O3K token. The native token must not outlive policy selected by the accepted specification. Avoid turning a short-lived/revoked federation credential into an unexpectedly long-lived O3K credential.

Document refresh/re-exchange behavior. O3K does not need to store an IdP refresh token unless explicitly accepted; the Araf BFF remains responsible for browser/OIDC session handling.

## Error contract

Use native RFC 9457 Problem Details and stable machine codes. Failures should not enumerate whether an external subject, principal, project or assignment exists.

## Required tests

- valid federated token + authorized Project A -> native token;
- `/identity/me` returns canonical Project A AuthContext;
- wrong project denied;
- unknown binding denied;
- disabled binding/principal/project denied;
- expired/wrong-audience/wrong-issuer token denied;
- removed assignment denied on new exchange;
- existing native password/token issuance remains unchanged;
- Keystone-compatible token behavior remains unchanged;
- cross-project native API read/mutation remains denied;
- token contents/claims remain secret-safe in logs;
- restart behavior matches documented native-token persistence/key model.

## Idempotency/concurrency

Repeated valid exchanges may produce distinct native tokens unless the accepted token model says otherwise; they must always resolve to the same canonical principal/scope and must not create duplicate IAM authority records.

## Completion evidence

Provide exact wire examples with secrets redacted, OpenAPI/schema references, compatibility regression evidence and an end-to-end in-process federation-to-AuthContext test.

Verdict:

`P12-IAM.4 federated native token exchange: PASS|BLOCKED`
