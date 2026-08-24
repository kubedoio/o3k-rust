CREATE TABLE canonical_networks (
    id TEXT PRIMARY KEY NOT NULL,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state VARCHAR(32) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error'))
);

CREATE INDEX canonical_networks_project_idx ON canonical_networks(project_id);

CREATE TABLE canonical_address_realms (
    id TEXT PRIMARY KEY NOT NULL,
    network_id TEXT NOT NULL REFERENCES canonical_networks(id),
    project_id TEXT NOT NULL,
    prefix CIDR NOT NULL,
    overlapping_prefixes BOOLEAN NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state VARCHAR(32) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    UNIQUE (network_id, id)
);

CREATE INDEX canonical_address_realms_network_idx
    ON canonical_address_realms(network_id);
CREATE INDEX canonical_address_realms_project_idx
    ON canonical_address_realms(project_id);

CREATE TABLE canonical_address_pools (
    id TEXT PRIMARY KEY NOT NULL,
    realm_id TEXT NOT NULL REFERENCES canonical_address_realms(id),
    project_id TEXT NOT NULL,
    prefix CIDR NOT NULL,
    gateway INET,
    first_usable INET NOT NULL,
    last_usable INET NOT NULL,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state VARCHAR(32) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error'))
);

CREATE INDEX canonical_address_pools_realm_idx
    ON canonical_address_pools(realm_id);
CREATE INDEX canonical_address_pools_project_idx
    ON canonical_address_pools(project_id);

CREATE TABLE canonical_endpoints (
    id TEXT PRIMARY KEY NOT NULL,
    realm_id TEXT NOT NULL REFERENCES canonical_address_realms(id),
    project_id TEXT NOT NULL,
    fixed_ip INET NOT NULL,
    mac TEXT NOT NULL UNIQUE,
    generation BIGINT NOT NULL CHECK (generation > 0),
    state VARCHAR(32) NOT NULL CHECK (state IN ('requested', 'active', 'deleting', 'deleted', 'error')),
    UNIQUE (realm_id, fixed_ip)
);

CREATE INDEX canonical_endpoints_realm_idx
    ON canonical_endpoints(realm_id);
CREATE INDEX canonical_endpoints_project_idx
    ON canonical_endpoints(project_id);

CREATE TABLE canonical_realm_encapsulation_bindings (
    fabric_domain_id TEXT NOT NULL,
    realm_id TEXT NOT NULL REFERENCES canonical_address_realms(id),
    provider_kind TEXT NOT NULL,
    provider_segment_id BIGINT NOT NULL CHECK (provider_segment_id > 0),
    binding_generation BIGINT NOT NULL CHECK (binding_generation > 0),
    state VARCHAR(32) NOT NULL,
    PRIMARY KEY (fabric_domain_id, realm_id),
    UNIQUE (fabric_domain_id, provider_segment_id)
);

CREATE INDEX canonical_realm_bindings_realm_idx
    ON canonical_realm_encapsulation_bindings(realm_id);
