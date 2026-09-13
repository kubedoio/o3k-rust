-- Durable topology store (ADR-0181, SPEC-0038, P15.1 issue #931): canonical
-- regions, availability domains, the failure-domain hierarchy, and topology
-- bindings. Hierarchy validation, nesting ranks, and generation semantics are
-- owned by the kernel port; storage enforces uniqueness, referential
-- integrity, and optimistic-concurrency (CAS) generations.
CREATE TABLE IF NOT EXISTS topology_regions (
    id TEXT PRIMARY KEY NOT NULL,
    declared_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS topology_availability_domains (
    id TEXT PRIMARY KEY NOT NULL,
    region_id TEXT NOT NULL REFERENCES topology_regions(id),
    declared_at_ms BIGINT NOT NULL
);

CREATE TABLE IF NOT EXISTS failure_domains (
    id TEXT PRIMARY KEY NOT NULL,
    class TEXT NOT NULL,
    name TEXT NOT NULL,
    availability_domain_id TEXT NOT NULL REFERENCES topology_availability_domains(id),
    parent_id TEXT REFERENCES failure_domains(id),
    generation BIGINT NOT NULL,
    metadata TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_failure_domains_parent ON failure_domains(parent_id);
CREATE INDEX IF NOT EXISTS idx_failure_domains_az ON failure_domains(availability_domain_id);

-- Bindings are the only topology edges to the rest of the system and are
-- unique per (failure domain, target kind, target id).
CREATE TABLE IF NOT EXISTS topology_bindings (
    failure_domain_id TEXT NOT NULL REFERENCES failure_domains(id),
    target_kind TEXT NOT NULL,
    target_id TEXT NOT NULL,
    created_at_ms BIGINT NOT NULL,
    PRIMARY KEY (failure_domain_id, target_kind, target_id)
);
CREATE INDEX IF NOT EXISTS idx_topology_bindings_target ON topology_bindings(target_kind, target_id);
