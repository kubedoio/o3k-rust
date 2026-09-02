# P12-IAM.5 — System / Operator AuthContext and Authorization

## Goal

Implement an explicit canonical system/operator authorization path suitable for the Araf Operator Console without username magic, fixture identity or implicit project-admin escalation.

## Preconditions

- P12-IAM.4 merged.
- Start from latest protected `main`.

## Architecture

Use canonical O3K scope/action/resource authorization. If `ScopeKind::System` exists in the kernel, complete its actual IAM semantics rather than treating the enum alone as implementation.

An operator principal must receive system-level access only through durable, explicit O3K authorization state and policy evaluation.

Never implement patterns such as:

```text
username == "admin"
email domain == operator domain
member role in one project => system admin
Araf operator session => implicit superuser
```

## Required capabilities

Define the bounded system/operator profile needed for the production Operator Console, including the exact actions/resources it may access. Prefer least privilege and named canonical O3K ActionIds.

At minimum prove the distinction between:

- tenant project principal;
- project administrator where supported;
- system/operator principal;
- service principal/delegated service identity.

System scope must not silently broaden service-principal delegation or tenant user scope.

## Token/AuthContext

Allow an explicitly authorized federated operator to request/receive an appropriate native O3K system-scoped token/AuthContext according to the accepted IAM spec.

The resulting `AuthContext` must remain the same canonical type consumed by protected services/API adapters.

## Operator API boundary

Audit the current operator/system API surface. Every operator-only endpoint must declare and enforce an explicit authorization contract. An authenticated tenant token discovering the route URL must still fail server-side.

Do not add data-plane shell/SSH/kubectl escape hatches as part of operator access.

## Required tests

- explicit operator binding/assignment -> system AuthContext PASS;
- normal tenant -> system token DENIED;
- project admin -> system token DENIED unless separately assigned;
- Alice tenant token -> operator API DENIED;
- operator token -> selected operator API PASS;
- operator token does not bypass resource/action policies outside selected profile;
- service principal does not become system principal;
- removed/disabled operator assignment denies new system exchange;
- restart preserves authorization state;
- SQLite/PostgreSQL parity where applicable;
- audit records original principal, system scope, action and decision without credentials.

## Break-glass

If emergency/break-glass access exists or is required, define it as a separate bounded operational profile with explicit audit and expiry. Do not make it the normal Operator Console authentication path.

## Completion evidence

Provide the operator capability matrix, tested routes, cross-boundary negative evidence and audit samples with redaction.

Verdict:

`P12-IAM.5 system/operator authorization: PASS|BLOCKED`
