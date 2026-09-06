# P12-IAM.6 — Public IAM contract gate

## Authoritative artifacts

The versioned native IAM contract is
[`contracts/openapi/native-iam.yaml`](../contracts/openapi/native-iam.yaml),
OpenAPI 3.1.2 / JSON Schema 2020-12. It covers:

| Operation | Contract behavior |
| --- | --- |
| `issueNativeToken` | Password, native-token, project-federated, and explicit system-federated request shapes; 201 response; stable Problem Details failures |
| `getNativeIdentityContext` | Bearer-authenticated public-safe canonical principal and effective scope projection |
| `getOperatorProfile` | Bearer-authenticated, server-authorized bounded `operator-console` system profile |

Federated scope discovery remains an IAM/domain-library capability in this
slice and is not advertised as a native HTTP route because no native discovery
route is implemented. The contract therefore cannot imply a client-visible
scope-discovery endpoint that does not exist.

## Compatibility and security policy

The native contract is versioned independently from the bootstrap OpenStack
contract. Additive optional response fields and new documented error details
are compatible. Removing an operation, changing a required request field,
changing the meaning of a scope, changing a stable error code/status, or
broadening/narrowing system authorization is breaking and requires a new API
version or an accepted compatibility decision.

The OIDC issuer, audience, discovery/JWKS configuration, client credentials,
assignment records, and token contents remain server-side. Examples use only
the literal `REDACTED_EXTERNAL_TOKEN`; schemas mark credential fields as
write-only. Public projections contain durable IDs and safe display values,
never raw credentials or policy internals.

## Evidence

`tests/native-iam-contract.sh` checks the exact implemented route set,
operation IDs, OpenAPI version/policy, bearer security, stable error-code
inventory, local references, redacted request examples, and JSON Schema
validation. It also runs `openapi-typescript@7.8.0` to prove a standard
generated-client smoke path. CI runs this check beside the existing OpenAPI
governance gate, so native route/contract drift fails deterministically.
