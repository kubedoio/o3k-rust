CREATE TABLE IF NOT EXISTS placement_providers (
    id TEXT PRIMARY KEY NOT NULL,
    node_id TEXT NOT NULL UNIQUE,
    state TEXT NOT NULL,
    generation INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS placement_inventories (
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    total INTEGER NOT NULL,
    reserved INTEGER NOT NULL,
    allocation_ratio REAL NOT NULL,
    used INTEGER NOT NULL,
    PRIMARY KEY (provider_id, resource_class)
);

CREATE TABLE IF NOT EXISTS placement_allocations (
    id TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    consumer_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_placement_allocations_provider ON placement_allocations(provider_id);
CREATE INDEX IF NOT EXISTS idx_placement_allocations_consumer ON placement_allocations(consumer_id);

CREATE TABLE IF NOT EXISTS placement_allocation_resources (
    allocation_id TEXT NOT NULL REFERENCES placement_allocations(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    amount INTEGER NOT NULL,
    PRIMARY KEY (allocation_id, resource_class)
);

CREATE TABLE IF NOT EXISTS placement_allocation_intents (
    id TEXT PRIMARY KEY NOT NULL,
    provider_id TEXT NOT NULL REFERENCES placement_providers(id) ON DELETE CASCADE,
    consumer_id TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_placement_intents_provider ON placement_allocation_intents(provider_id);

CREATE TABLE IF NOT EXISTS placement_allocation_intent_resources (
    intent_id TEXT NOT NULL REFERENCES placement_allocation_intents(id) ON DELETE CASCADE,
    resource_class TEXT NOT NULL,
    amount INTEGER NOT NULL,
    PRIMARY KEY (intent_id, resource_class)
);
