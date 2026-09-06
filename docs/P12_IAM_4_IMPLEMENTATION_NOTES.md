# P12-IAM.4 implementation notes

Federated exchange is an explicit native credential method. The bounded native
request shape is:

```json
{
  "auth": {
    "method": "federated",
    "federated": { "access_token": "<external access token>" },
    "project_id": "<canonical project id>"
  }
}
```

The native `password` and `token` methods retain their existing semantics.
Federated exchange is not represented as native token reauthentication and does
not add refresh-token handling to O3K. The browser/BFF retains its external
provider refresh credential and may re-exchange a fresh access token.

The `o3kd` composition layer enables federation only when all four settings are
present: `O3K_OIDC_TRUST_ID`, `O3K_OIDC_ISSUER`, `O3K_OIDC_AUDIENCE`, and
`O3K_OIDC_DISCOVERY_URL`. Partial configuration fails startup. The current
composition profile permits only HTTPS issuer/discovery URLs and RS256 signing;
local/insecure test validators are constructed explicitly by tests, not by
production environment configuration.

The adapter validates the external access token against the configured issuer,
audience, signature, time claims, and discovery JWKS, then passes only the
validated canonical binding claims and external expiry into IAM. Raw external
credentials are not persisted or placed in `AuthContext`; request and
credential debug output redacts password, native token, and external token
values. IAM rechecks the durable binding, project assignment, and enabled state
before issuing the existing native token format. Native token expiry is the
minimum of the configured native TTL and the validated external expiry.

Successful exchange and native token reauthentication produce the canonical
`AuthContext` consumed by downstream services. Invalid external credentials,
unknown bindings/scopes, disabled assignments, and expired external tokens map
to non-enumerating RFC 9457 unauthorized failures; absent federation
configuration is reported as service unavailability.

This slice proves the domain exchange/lifetime contract and explicit wire
credential validation. Public OpenAPI publication, operator/system scope
selection, and real-provider evidence remain owned by P12-IAM.5 through .7.
