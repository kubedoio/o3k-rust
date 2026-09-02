# P12-IAM.2 — Durable Federated Subject-to-Principal Binding

## Goal

Bind a validated external OIDC identity to one stable canonical O3K principal without making IdP display attributes authoritative.

## Preconditions

- P12-IAM.1 merged.
- Start from current protected `main`.

## Canonical identity rule

External identity key:

```text
trusted_issuer_id / canonical issuer URL
+
OIDC subject (`sub`)
```

maps to:

```text
O3K PrincipalId
```

The binding is durable O3K IAM state.

## Required model

Add the smallest durable representation required by the accepted P12-IAM ADR/spec. It must support at minimum:

- stable binding ID if the repository model requires one;
- trusted issuer identity;
- subject;
- O3K principal ID/type;
- enabled/disabled state or equivalent lifecycle behavior;
- created/updated audit metadata as appropriate;
- uniqueness preventing ambiguous `(issuer, subject)` resolution;
- referential integrity to the canonical O3K principal where practical.

Use migrations for SQLite/PostgreSQL according to repository conventions.

## Provisioning boundary

For the first production profile prefer explicit/pre-provisioned bindings unless P12-IAM.0 explicitly accepted JIT provisioning.

Do not silently create privileged O3K users because an unknown external subject successfully authenticated at the IdP.

Unknown binding must fail closed with a non-enumerating authentication/authorization error.

## Attribute policy

Optional IdP claims such as email/name may be retained only as bounded display/audit metadata if the accepted design permits it. They must not be used to select an O3K principal or authorization scope.

Changing email/display name at the IdP must not change canonical identity.

## Required tests

- same `(issuer, sub)` resolves same PrincipalId;
- same `sub` at different trusted issuer does not collide;
- same email for two subjects does not collide;
- duplicate binding insertion rejected;
- disabled binding denied;
- missing O3K principal denied/fails integrity checks;
- disabled O3K principal denied;
- restart/reload preserves mapping;
- SQLite/PostgreSQL parity;
- concurrent binding creation cannot create ambiguity;
- deletion/disable behavior is explicit and safe;
- raw access token is never persisted.

## Security/audit

Audit should be able to correlate the canonical O3K principal and external issuer/subject identity using safe identifiers, while excluding raw credentials.

## Non-goals

- IdP group synchronization;
- SCIM;
- email-domain auto-enrollment;
- JIT privileged operator creation;
- browser UI for managing bindings unless separately selected.

## Completion evidence

Provide migration/model locations, uniqueness/foreign-key proof, restart evidence, cross-backend evidence and negative-security results.

Verdict:

`P12-IAM.2 federated binding: PASS|BLOCKED`
