# A1 Native Read Runtime contract

The initial native Operations collection intentionally exposes only the
canonical owner-scoped collection ordered by durable operation UUID.  It does
not advertise state, service, action, resource, or time filters until those
filters have an authoritative indexed query contract.  Unknown query fields
are rejected.  Relationships use the same bounded `ResourceQuery`/
`ResourcePage` contract and cursor authority as resource collections.

This filterless scope is deliberate: clients must not infer that an
unimplemented filter was applied, and future filters require a versioned
contract plus repository-bounded evidence.
