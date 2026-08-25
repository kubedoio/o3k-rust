-- Canonical AddressRealms have their own authoritative table.  The generic
-- resources row is only the shared Operation foreign-key/index surface; it is
-- not a second desired-state authority.
INSERT INTO resources (
    id, kind, project_id, generation, observed_generation,
    desired_state, observed_state, provider_id
)
SELECT id, 'network:address_realm', project_id, generation, generation,
       state, state, NULL
FROM canonical_address_realms
