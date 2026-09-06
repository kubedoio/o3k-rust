# P12-IAM.5 — System/operator AuthContext and authorization

## Profile and authority

This slice implements the bounded `operator-console` profile in the native
O3K Cloud OS IAM path. O3K IAM is authoritative for the system-scoped token,
the durable operator assignment, and the resulting `AuthContext`. No external
cloud or execution provider is involved.

System scope is issued only when a validated external identity resolves to a
canonical enabled human user with an enabled durable assignment for the exact
`operator-console` profile. Usernames, display names, project-admin roles,
client input, browser state, and IdP group claims do not grant system access.
Federated bindings marked as service principals are rejected by this path.

## Public contract

Native token issuance accepts the explicit system form:

```json
{
  "auth": {
    "method": "federated",
    "federated": {
      "access_token": "<external-token>",
      "scope": { "kind": "system" }
    }
  }
}
```

The server selects `operator-console` and issues a native token whose
`AuthContext` has `ScopeKind::System`, a human `Principal`, and only the
`operator` role. Project-scoped federated credentials remain project-only.

The only operator route in this slice is `GET /o3k/v1/operator/profile`.
It authorizes the server-side `operator:ReadProfile` action against the
`operator:profile` resource collection owned by the `system` scope. The route
returns profile/scope identity and audit correlation only; it provides no
shell, host, data-plane, or arbitrary action access.

## Durable state and restart

Assignments are stored in `operator_assignments` with a canonical user foreign
key, explicit profile, enabled flag, timestamps, and a unique
`(user_id, profile)` constraint. SQLite and PostgreSQL migrations and identity
repositories are implemented. Identity snapshot loading includes assignments,
so restart does not widen or erase operator authority.

## Evidence and non-goals

Tests cover explicit system credential parsing, assignment-required federated
exchange, external-expiry bounding, system `AuthContext` construction,
service-principal rejection, project-scope denial, and kernel authorization
requiring a system human with the operator role. This slice does not claim a
general administrative console, shell access, host control, or arbitrary
system-wide service mutation.
