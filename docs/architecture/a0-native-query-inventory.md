# A0 native query/discovery inventory

This is the checked-in inventory for the Native Query and Discovery Foundation.
The verifier requires every row advertised by the manifest to have an explicit
bounded adapter and runtime readiness evidence. `unknown` is a failure, not an
implicit support claim.

| Resource | Native collection | List declared | Bounded authority | Runtime readiness | Cursor evidence |
|---|---|---:|---|---|---|
| Compute server | `/compute/servers` | verify manifest | verify adapter | live controller | A0 verifier |
| Flavor | `/compute/flavors` | verify manifest | verify adapter | live controller | A0 verifier |
| Image | `/image/images` | verify manifest | verify adapter | live controller | A0 verifier |
| Network | `/network/networks` | verify manifest | verify adapter | live controller | A0 verifier |
| Canonical network | `/network/address-realms` | verify manifest | verify adapter | live controller | A0 verifier |
| Subnet | `/network/subnets` | verify manifest | verify adapter | live controller | A0 verifier |
| Port | `/network/ports` | verify manifest | verify adapter | live controller | A0 verifier |
| Router | `/network/routers` | verify manifest | verify adapter | live controller | A0 verifier |
| Security group | `/network/security-groups` | verify manifest | verify adapter | live controller | A0 verifier |
| Floating IP | `/network/floating-ips` | verify manifest | verify adapter | live controller | A0 verifier |
| Volume | `/volume/volumes` | verify manifest | verify adapter | live controller | A0 verifier |
| Volume attachment | `/volume/attachments` | verify manifest | verify adapter | live controller | A0 verifier |

The resource registry is authoritative for the final advertised set; this
table deliberately does not grant support to resources absent from it.

