CREATE TABLE IF NOT EXISTS network_address_allocations (
    realm_id TEXT NOT NULL,
    project_id TEXT NOT NULL,
    endpoint_id TEXT PRIMARY KEY NOT NULL,
    operation_id TEXT NOT NULL UNIQUE,
    address TEXT NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (realm_id, address)
);

CREATE INDEX IF NOT EXISTS idx_network_address_allocations_realm
    ON network_address_allocations(realm_id);
