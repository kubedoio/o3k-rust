CREATE TABLE IF NOT EXISTS network_address_allocations (
    realm_id VARCHAR(36) NOT NULL,
    project_id VARCHAR(255) NOT NULL,
    endpoint_id VARCHAR(36) PRIMARY KEY NOT NULL,
    operation_id VARCHAR(255) NOT NULL UNIQUE,
    address INET NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (realm_id, address)
);

CREATE INDEX IF NOT EXISTS idx_network_address_allocations_realm
    ON network_address_allocations(realm_id);
