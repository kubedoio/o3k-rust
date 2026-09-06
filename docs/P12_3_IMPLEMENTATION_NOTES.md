# P12.3 implementation notes

Scope discovery is a domain contract in this slice; public route and schema
selection remains deferred to P12-IAM.6. `TokenService::discover_federated_scopes`
accepts only a validated external identity and resolves its explicitly
provisioned `(trusted_issuer_id, issuer, subject)` binding to the canonical O3K
user. It projects enabled project assignments with enabled domains, stable IDs,
safe names, and `can_request_token`; domain and system scopes are not
advertised because their authorization semantics are not yet implemented.

`TokenService::authorize_federated_scope` is the rescoping decision boundary.
It independently revalidates the binding, canonical user, requested project,
domain state, and current role assignment. It never copies a prior
`AuthContext`, accepts a caller-supplied project as authority, or distinguishes
missing identity, project, or assignment state through the unauthorized result.

Negative isolation matrix:

| Case | Result |
| --- | --- |
| Alice bound to Project A | Project A is discoverable and requestable |
| Bob bound to Project B | Project B is discoverable and requestable |
| Alice requests Project B or an arbitrary ID | Non-enumerating unauthorized denial |
| Disabled project/domain or removed role assignment | Omitted and unauthorized |
| Duplicate project display names | Selection remains by stable project ID |
| Normal tenant requests domain/system scope | No such scope is advertised or authorized |

Bindings are loaded from the durable repository into the restart-loaded
identity snapshot, while scope truth is derived from the same canonical
project, domain, user, role, and assignment records used by native issuance.
SQLite and PostgreSQL expose the same binding-listing port for this snapshot
load; route-level pagination is deferred until the public API slice selects the
bounded wire contract.
