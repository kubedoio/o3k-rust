# A0 native collection inventory

This matrix is generated from the manifest resource set and the production
`GenericResourceApplication` capability gate. List is advertised only when
the manifest declares it, the owning controller is live Ready, and the adapter
returns a bounded repository page. Suppressed rows never advertise List.

| Resource type | Collection | List status | Bounded authority | Ordering/cursor |
|---|---|---|---|---|
| image:image | image | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| compute:server | servers | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| compute:flavor | flavor | suppressed | no bounded adapter | not advertised |
| compute:keypair | keypair | suppressed | no manifest List operation | not advertised |
| network:address_realm | address-realms | suppressed | no bounded adapter | not advertised |
| network:network | networks | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:subnet | subnets | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:port | ports | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:security_group | security-groups | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:security_group_rule | security-group-rules | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:router | routers | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:router_interface | router-interfaces | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| network:floating_ip | floating-ips | suppressed | no bounded adapter | not advertised |
| volume:volume | volumes | advertised when Ready | `resources` keyset query (`LIMIT N+1`) | `id.asc`, opaque |
| volume:volume_attachment | volume_attachment | suppressed | no bounded adapter | not advertised |

The native handler delegates query validation and page construction to the
application boundary. Compatibility routes are separate protocol adapters and
are not native collection authorities.

A0 intentionally supports no client filters and only the canonical `id.asc`
ordering. Unknown query parameters and future filter/order values are rejected;
the opaque cursor is bound to this fixed query identity (`filters:none`,
`order:id.asc`) in addition to scope and resource type. Extending the query
vocabulary requires a versioned contract change.
